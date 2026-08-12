import type {
  Message,
  ProviderUsage,
  RenderedContext,
  StreamEvent,
  TextDelta,
  ToolCall,
  ToolCallEvent,
  ToolSchema,
  UsageEvent,
} from "../types.js"
import type {
  CanonicalAdapterInput,
  CanonicalMessage,
  CanonicalToolResult,
} from "./content-normalization.js"
import { normalizeCanonicalContext } from "./content-normalization.js"
import { normalizeToolCall, UnsupportedModalityError } from "./base.js"
import {
  type AdapterDecodeInput,
  type AdapterOutput,
  type AdapterStreamInput,
  type CanonicalStopReason,
  type ProtocolAdapter,
  ProtocolResponseError,
} from "./protocol-adapter.js"
import { OPENAI_RESPONSES_PROTOCOL_CAPABILITIES } from "./protocol-capabilities.js"

export interface OpenAIResponsesRunState {
  previousResponseId?: string
  coveredMessageCount: number
  [key: string]: unknown
}

export interface OpenAIResponsesRequestPlan {
  params: Record<string, unknown>
}

export type OpenAIResponsesStreamChunk = Record<string, any>

export interface OpenAIResponsesStreamState {
  readonly input: CanonicalAdapterInput
  readonly functionCalls: Map<number, { id: string; name: string; argsBuffer: string }>
}

function numberField(raw: Record<string, unknown>, field: string): number | undefined {
  const value = raw[field]
  if (value === undefined || value === null) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new ProtocolResponseError("openai-responses", `usage.${field} must be a non-negative finite number`)
  }
  return value
}

function cacheReadTokens(usage: Record<string, unknown>): number | undefined {
  const inputDetails = usage.input_tokens_details
  if (inputDetails === undefined) return undefined
  if (!inputDetails || typeof inputDetails !== "object" || Array.isArray(inputDetails)) {
    throw new ProtocolResponseError("openai-responses", "usage.input_tokens_details must be an object")
  }
  return numberField(inputDetails as Record<string, unknown>, "cached_tokens")
}

function toolResultOutput(result: CanonicalToolResult): string | Array<Record<string, unknown>> {
  const output: Array<Record<string, unknown>> = []
  for (const block of result.blocks) {
    if (block.type === "text") {
      output.push({ type: "input_text", text: block.text })
    } else if (block.type === "image") {
      if (block.source.kind === "base64") {
        output.push({
          type: "input_image",
          image_url: `data:${block.mediaType ?? "image/png"};base64,${block.source.data}`,
        })
      } else if (block.source.kind === "url") {
        output.push({ type: "input_image", image_url: block.source.url })
      } else {
        output.push({ type: "input_text", text: "[image]" })
      }
    } else {
      output.push({ type: "input_text", text: `[${block.type}]` })
    }
  }
  return result.contentForm !== "blocks" && output.length === 1 && output[0].type === "input_text"
    ? String(output[0].text ?? "")
    : output
}

function messageContent(message: CanonicalMessage): string | Array<Record<string, unknown>> {
  const content: Array<Record<string, unknown>> = []
  for (const block of message.blocks) {
    if (block.type === "tool_result") continue
    if (block.type === "text") {
      content.push({ type: "input_text", text: block.text })
    } else if (block.type === "image") {
      const imageUrl = block.source.kind === "url"
        ? block.source.url
        : block.source.kind === "base64"
          ? `data:${block.mediaType ?? "image/png"};base64,${block.source.data}`
          : undefined
      if (imageUrl) {
        content.push({
          type: "input_image",
          detail: String(block.providerOptions?.openai_detail ?? "auto"),
          image_url: imageUrl,
        })
      }
    } else if (block.type === "audio") {
      throw new UnsupportedModalityError("audio", "openai-responses")
    }
  }
  return message.contentForm !== "blocks" && content.length === 1 && content[0].type === "input_text"
    ? String(content[0].text ?? "")
    : content
}

function hasMessageContent(message: CanonicalMessage): boolean {
  return message.blocks.some(block =>
    block.type !== "tool_result" && (block.type !== "text" || block.text.length > 0),
  )
}

function appendMessage(input: Array<Record<string, unknown>>, message: CanonicalMessage): void {
  if (message.role === "assistant" && message.toolCalls?.length) {
    if (hasMessageContent(message)) {
      input.push({ role: "assistant", content: messageContent(message) })
    }
    for (const call of message.toolCalls) {
      input.push({
        type: "function_call",
        call_id: call.id,
        name: call.name,
        arguments: call.arguments,
      })
    }
    return
  }
  if (message.role === "tool") {
    for (const block of message.blocks) {
      if (block.type !== "tool_result") continue
      input.push({
        type: "function_call_output",
        call_id: block.callId,
        output: toolResultOutput(block),
      })
    }
    return
  }
  input.push({ role: message.role, content: messageContent(message) })
}

function canonicalInputItems(
  context: CanonicalAdapterInput["context"],
  state?: OpenAIResponsesRunState,
): Array<Record<string, unknown>> {
  const input: Array<Record<string, unknown>> = []
  const turns = state?.previousResponseId
    ? context.turns.slice(state.coveredMessageCount)
    : context.turns
  for (const message of turns) appendMessage(input, message)
  if (context.stateTurn) appendMessage(input, context.stateTurn)
  return input
}

function requestExtensions(extensions: Readonly<Record<string, unknown>>): Record<string, unknown> {
  const blocked = new Set([
    "model", "input", "instructions", "tools", "stream", "previous_response_id",
    "web_search", "builtin_tools", "__deepstrikeThinkingEnabled", "degradeMissingReasoningReplay",
  ])
  return Object.fromEntries(Object.entries(extensions).filter(([key]) => !blocked.has(key)))
}

function builtinTools(extensions: Readonly<Record<string, unknown>>): Record<string, unknown>[] {
  const output: Record<string, unknown>[] = []
  if (extensions.web_search) {
    output.push(typeof extensions.web_search === "object"
      ? { type: "web_search", ...extensions.web_search as Record<string, unknown> }
      : { type: "web_search" })
  }
  if (Array.isArray(extensions.builtin_tools)) {
    output.push(...extensions.builtin_tools as Record<string, unknown>[])
  }
  return output
}

function decodeOutput(output: Array<Record<string, unknown>>): {
  content: string
  toolCalls: ToolCall[]
} {
  let content = ""
  const toolCalls: ToolCall[] = []
  for (const item of output) {
    if (item.type === "message") {
      for (const part of item.content as Array<Record<string, unknown>> | undefined ?? []) {
        if (part.type === "output_text") content += String(part.text ?? "")
      }
    } else if (item.type === "function_call") {
      const call = normalizeToolCall(
        String(item.call_id ?? item.id ?? ""),
        String(item.name ?? ""),
        item.arguments ?? "{}",
      )
      if (call) toolCalls.push(call)
    }
  }
  return { content, toolCalls }
}

export class OpenAIResponsesAdapter implements ProtocolAdapter<
  OpenAIResponsesRequestPlan,
  Record<string, any>,
  OpenAIResponsesStreamChunk,
  OpenAIResponsesStreamState,
  undefined
> {
  readonly protocol = "openai-responses" as const
  readonly protocolCapabilities = OPENAI_RESPONSES_PROTOCOL_CAPABILITIES

  buildRequest(
    input: CanonicalAdapterInput,
    state?: OpenAIResponsesRunState,
  ): OpenAIResponsesRequestPlan {
    const functionTools = this.buildTools(input.tools)
    const tools = [...functionTools, ...builtinTools(input.extensions)]
    return {
      params: {
        ...requestExtensions(input.extensions),
        model: input.resolved.identity.modelId,
        input: canonicalInputItems(input.context, state),
        ...(input.context.systemText ? { instructions: input.context.systemText } : {}),
        ...(state?.previousResponseId ? { previous_response_id: state.previousResponseId } : {}),
        ...(tools.length ? { tools } : {}),
      },
    }
  }

  decodeComplete(raw: Record<string, any>, _input: AdapterDecodeInput): { message: Message } {
    const decoded = decodeOutput(raw.output ?? [])
    const usage = raw.usage && typeof raw.usage === "object"
      ? raw.usage as Record<string, unknown>
      : undefined
    if (usage) this.normalizeUsage(usage)
    const tokenCount = usage
      ? numberField(usage, "output_tokens") ?? numberField(usage, "total_tokens")
      : undefined
    return {
      message: {
        role: "assistant",
        content: decoded.content,
        toolCalls: decoded.toolCalls,
        ...(tokenCount !== undefined ? { tokenCount } : {}),
      },
    }
  }

  createStreamState(
    input: AdapterStreamInput,
    _state?: OpenAIResponsesRunState,
  ): OpenAIResponsesStreamState {
    return { input: input.input, functionCalls: new Map() }
  }

  pushStreamChunk(
    chunk: OpenAIResponsesStreamChunk,
    state: OpenAIResponsesStreamState,
  ): AdapterOutput {
    const events: StreamEvent[] = []
    let runStatePatch: Partial<OpenAIResponsesRunState> | undefined
    if (chunk.type === "response.output_text.delta") {
      events.push({ type: "text_delta", delta: chunk.delta } as TextDelta)
    } else if (chunk.type === "response.output_item.added" && chunk.item.type === "function_call") {
      state.functionCalls.set(chunk.output_index, {
        id: chunk.item.call_id,
        name: chunk.item.name,
        argsBuffer: chunk.item.arguments ?? "",
      })
    } else if (chunk.type === "response.function_call_arguments.delta") {
      const call = state.functionCalls.get(chunk.output_index)
      if (call) call.argsBuffer += chunk.delta
    } else if (chunk.type === "response.function_call_arguments.done") {
      const call = state.functionCalls.get(chunk.output_index)
      if (call) call.argsBuffer = chunk.arguments
    } else if (chunk.type === "response.output_item.done" && chunk.item.type === "function_call") {
      const call = state.functionCalls.get(chunk.output_index) ?? {
        id: chunk.item.call_id,
        name: chunk.item.name,
        argsBuffer: chunk.item.arguments ?? "{}",
      }
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(call.argsBuffer || "{}") as Record<string, unknown> } catch { args = {} }
      events.push({ type: "tool_call", id: call.id, name: call.name, arguments: args } as ToolCallEvent)
    } else if (chunk.type === "response.completed" || chunk.type === "response.incomplete") {
      const response = chunk.response as Record<string, any>
      runStatePatch = {
        previousResponseId: String(response.id),
        coveredMessageCount: state.input.context.turns.length + 1,
      }
      const usage = response.usage as Record<string, unknown> | undefined
      if (usage) {
        const totalTokens = numberField(usage, "total_tokens")
        const providerUsage = this.normalizeUsage(usage)
        const cacheReadInputTokens = cacheReadTokens(usage)
        const inputTokens = numberField(usage, "input_tokens")
        const outputTokens = numberField(usage, "output_tokens")
        const rawStopReason = typeof response.incomplete_details?.reason === "string"
          ? response.incomplete_details.reason
          : undefined
        const stopReason = this.normalizeStopReason(rawStopReason)
        if (totalTokens && totalTokens > 0) {
          events.push({
            type: "usage",
            totalTokens,
            ...(inputTokens ? { inputTokens } : {}),
            ...(outputTokens ? { outputTokens } : {}),
            ...(cacheReadInputTokens ? { cacheReadInputTokens } : {}),
            ...(providerUsage && (inputTokens || outputTokens) ? { providerUsage } : {}),
            ...(stopReason ? { stopReason } : {}),
            ...(rawStopReason ? { rawStopReason } : {}),
          } as UsageEvent)
        }
      }
    }
    return { events, ...(runStatePatch ? { runStatePatch } : {}) }
  }

  finishStream(_state: OpenAIResponsesStreamState): AdapterOutput {
    return { events: [] }
  }

  normalizeUsage(raw: unknown): ProviderUsage | undefined {
    if (raw === undefined || raw === null) return undefined
    if (typeof raw !== "object" || Array.isArray(raw)) {
      throw new ProtocolResponseError("openai-responses", "usage must be an object")
    }
    const usage = raw as Record<string, unknown>
    const inputTokens = numberField(usage, "input_tokens")
    const outputTokens = numberField(usage, "output_tokens")
    numberField(usage, "total_tokens")
    const outputDetails = usage.output_tokens_details
    if (outputDetails !== undefined && (!outputDetails || typeof outputDetails !== "object" || Array.isArray(outputDetails))) {
      throw new ProtocolResponseError("openai-responses", "usage.output_tokens_details must be an object")
    }
    cacheReadTokens(usage)
    const reasoningTokens = outputDetails
      ? numberField(outputDetails as Record<string, unknown>, "reasoning_tokens")
      : undefined
    if (inputTokens === undefined && outputTokens === undefined) return undefined
    return {
      inputTokens: inputTokens ?? 0,
      outputTokens: outputTokens ?? 0,
      ...(reasoningTokens !== undefined ? { reasoningTokens } : {}),
    }
  }

  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined {
    if (raw === undefined) return undefined
    if (raw === "max_output_tokens") return "max_tokens"
    if (raw === "content_filter") return "content_filter"
    return "other"
  }

  // Published compatibility helpers retained while internally routing through canonical input.
  buildTools(tools: readonly ToolSchema[]): Array<Record<string, unknown>> {
    return tools.map(tool => ({
      type: "function",
      name: tool.name,
      description: tool.description,
      parameters: JSON.parse(tool.parameters),
    }))
  }

  buildInstructions(context: RenderedContext): string | undefined {
    return context.systemText || undefined
  }

  buildInput(context: RenderedContext, state?: OpenAIResponsesRunState): Array<Record<string, unknown>> {
    return canonicalInputItems(normalizeCanonicalContext(context), state)
  }

  decodeOutput(output: Array<Record<string, unknown>>): {
    content: string
    toolCalls: ToolCall[]
  } {
    return decodeOutput(output)
  }

  builtinTools(extensions?: Record<string, unknown>): Record<string, unknown>[] {
    return builtinTools(extensions ?? {})
  }

  requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return requestExtensions(extensions ?? {})
  }
}
