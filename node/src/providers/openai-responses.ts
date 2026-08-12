import OpenAI from "openai"
import type { Message, ProviderRunState, RenderedContext, StreamEvent, TextDelta, ToolCallEvent, ToolResultPart, ToolSchema, LLMProvider } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys } from "./base.js"
import { normalizeToolCall } from "./base.js"
import { UnsupportedModalityError } from "./base.js"
import { normalizeOpenAIUsage } from "./usage-normalizer.js"
import { normalizeToolResultPart } from "./content-normalization.js"

export interface OpenAIResponsesRunState extends ProviderRunState {
  previousResponseId?: string
  coveredMessageCount: number
}

/**
 * spc_012-N-04: the Responses API natively accepts structured `function_call_output.output`
 * content arrays (`input_text` / `input_image`) — verified against OpenAI's function-calling
 * update ("images and files as function call outputs"). Structured `contentParts` therefore
 * serialize natively here; anything without a Responses wire form (audio, `fileId`/`object`
 * sources, nested tool_result) degrades to an explicit `[modality]` placeholder text item,
 * never silently dropped (INV-012-01).
 */
function toolResultResponsesOutput(part: ToolResultPart): string | Array<Record<string, unknown>> {
  if (part.contentParts === undefined) return part.output
  const out: Array<Record<string, unknown>> = []
  for (const block of normalizeToolResultPart(part).blocks) {
    if (block.type === "text") {
      out.push({ type: "input_text", text: block.text })
      continue
    }
    if (block.type === "image") {
      const src = block.source
      if (src.kind === "base64") {
        out.push({ type: "input_image", image_url: `data:${block.mediaType ?? "image/png"};base64,${src.data}` })
      } else if (src.kind === "url") {
        out.push({ type: "input_image", image_url: src.url })
      } else {
        out.push({ type: "input_text", text: "[image]" })
      }
      continue
    }
    out.push({ type: "input_text", text: `[${block.type}]` })
  }
  return out
}

export class OpenAIResponsesAdapter {
  buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function" as const,
      name: t.name,
      description: t.description,
      parameters: JSON.parse(t.parameters),
    }))
  }

  buildInstructions(context: RenderedContext): string | undefined {
    return context.systemText || undefined
  }

  buildInput(context: RenderedContext, state?: OpenAIResponsesRunState): Array<Record<string, unknown>> {
    const input: Array<Record<string, unknown>> = []
    const turns = context.turns
    const uncoveredMessages = state?.previousResponseId
      ? turns.slice(state.coveredMessageCount)
      : turns

    for (const message of uncoveredMessages) {
      if (message.role === "assistant" && message.toolCalls?.length) {
        if (message.content || message.contentParts?.length) {
          input.push({
            role: "assistant",
            content: this.buildMessageContent(message),
          })
        }
        for (const tc of message.toolCalls) {
          input.push({
            type: "function_call",
            call_id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
          })
        }
        continue
      }

      if (message.role === "tool") {
        for (const part of message.contentParts ?? []) {
          if (part.type !== "tool_result") continue
          input.push({
            type: "function_call_output",
            call_id: part.callId,
            output: toolResultResponsesOutput(part),
          })
        }
        continue
      }

      input.push({
        role: message.role,
        content: this.buildMessageContent(message),
      })
    }

    // The volatile State turn is sent every turn (it changes each call and is
    // never "covered" by previous_response_id). Absent on un-rebuilt bindings,
    // where the state is already inside the covered/uncovered history.
    // Rendered through the same assistant-toolCalls / tool-result branches above
    // so tool_use blocks are not silently dropped.
    if (context.stateTurn) {
      const st = context.stateTurn
      if (st.role === "assistant" && st.toolCalls?.length) {
        if (st.content || st.contentParts?.length) {
          input.push({ role: "assistant", content: this.buildMessageContent(st) })
        }
        for (const tc of st.toolCalls) {
          input.push({ type: "function_call", call_id: tc.id, name: tc.name, arguments: tc.arguments })
        }
      } else if (st.role === "tool") {
        for (const part of st.contentParts ?? []) {
          if (part.type !== "tool_result") continue
          input.push({ type: "function_call_output", call_id: part.callId, output: toolResultResponsesOutput(part) })
        }
      } else {
        input.push({ role: st.role, content: this.buildMessageContent(st) })
      }
    }

    return input
  }

  decodeOutput(output: Array<Record<string, unknown>>): {
    content: string
    toolCalls: Array<{ id: string; name: string; arguments: string }>
  } {
    let content = ""
    const toolCalls: Array<{ id: string; name: string; arguments: string }> = []

    for (const item of output) {
      if (item.type === "message") {
        for (const part of (item.content as Array<Record<string, unknown>> | undefined) ?? []) {
          if (part.type === "output_text") content += String(part.text ?? "")
        }
      } else if (item.type === "function_call") {
        const toolCall = normalizeToolCall(
          String(item.call_id ?? item.id ?? ""),
          String(item.name ?? ""),
          item.arguments ?? "{}",
        )
        if (toolCall) toolCalls.push(toolCall)
      }
    }

    return { content, toolCalls }
  }

  private buildMessageContent(message: Message): string | Array<Record<string, unknown>> {
    if (!message.contentParts?.length) return message.content

    const content: Array<Record<string, unknown>> = []
    for (const part of message.contentParts) {
      if (part.type === "text") {
        content.push({ type: "input_text", text: part.text })
        continue
      }
      if (part.type === "image") {
        // Default the MIME type (like every other serializer) so a data-only image is
        // not silently dropped; only a part with neither url nor data yields undefined.
        const imageUrl = part.url ?? (
          part.data
            ? `data:${part.mediaType ?? "image/png"};base64,${part.data}`
            : undefined
        )
        if (imageUrl) content.push({
          type: "input_image",
          detail: part.detail ?? "auto",
          image_url: imageUrl,
        })
        continue
      }
      if (part.type === "audio") {
        throw new UnsupportedModalityError("audio", "openai-responses")
      }
    }

    return content
  }
}

export class OpenAIResponsesProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly responses = new OpenAIResponsesAdapter()
  private readonly resolvedRuntimePolicy: import("../types.js").RuntimePolicy

  constructor(
    apiKey: string,
    protected readonly model = "gpt-4.1",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
    runtimePolicy: import("../types.js").RuntimePolicy = {},
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey, baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
    this.resolvedRuntimePolicy = runtimePolicy
  }

  runtimePolicy(): import("../types.js").RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  createRunState(): OpenAIResponsesRunState {
    return { coveredMessageCount: 0 }
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    let lastErr: unknown

    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const instructions = this.responses.buildInstructions(context)
        const resp = await this.client.responses.create({
          ...this.requestExtensions(extensions),
          model: this.model,
          input: this.responses.buildInput(context) as unknown as OpenAI.Responses.ResponseInput,
          ...(instructions ? { instructions } : {}),
          ...((t => t ? { tools: t } : {})(this.allTools(tools, extensions))),
        })
        this.circuit.recordSuccess()
        const decoded = this.responses.decodeOutput(resp.output as unknown as Array<Record<string, unknown>>)
        return {
          role: "assistant",
          content: decoded.content,
          toolCalls: decoded.toolCalls,
          tokenCount: resp.usage?.output_tokens ?? resp.usage?.total_tokens,
        }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }

    throw lastErr
  }

  async *stream(
    context: RenderedContext,
    tools: ToolSchema[],
    extensions?: Record<string, unknown>,
    state?: ProviderRunState,
  ): AsyncIterable<StreamEvent> {
    const runState = this.asRunState(state)
    const functionCalls = new Map<number, { id: string; name: string; argsBuf: string }>()
    const instructions = this.responses.buildInstructions(context)

    const stream = await this.client.responses.create({
      ...this.requestExtensions(extensions),
      model: this.model,
      input: this.responses.buildInput(context, runState) as unknown as OpenAI.Responses.ResponseInput,
      ...(instructions ? { instructions } : {}),
      ...(runState.previousResponseId ? { previous_response_id: runState.previousResponseId } : {}),
      ...((t => t ? { tools: t } : {})(this.allTools(tools, extensions))),
      stream: true,
    })

    for await (const evt of stream) {
      if (evt.type === "response.output_text.delta") {
        yield { type: "text_delta", delta: evt.delta } as TextDelta
      } else if (evt.type === "response.output_item.added" && evt.item.type === "function_call") {
        functionCalls.set(evt.output_index, {
          id: evt.item.call_id,
          name: evt.item.name,
          argsBuf: evt.item.arguments ?? "",
        })
      } else if (evt.type === "response.function_call_arguments.delta") {
        const call = functionCalls.get(evt.output_index)
        if (call) call.argsBuf += evt.delta
      } else if (evt.type === "response.function_call_arguments.done") {
        const call = functionCalls.get(evt.output_index)
        if (call) call.argsBuf = evt.arguments
      } else if (evt.type === "response.output_item.done" && evt.item.type === "function_call") {
        const call = functionCalls.get(evt.output_index) ?? {
          id: evt.item.call_id,
          name: evt.item.name,
          argsBuf: evt.item.arguments ?? "{}",
        }
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(call.argsBuf || "{}") } catch { args = {} }
        yield { type: "tool_call", id: call.id, name: call.name, arguments: args } as ToolCallEvent
      } else if (evt.type === "response.completed") {
        runState.previousResponseId = evt.response.id
        runState.coveredMessageCount = context.turns.length + 1
        if (evt.response.usage?.total_tokens) {
          // Responses API reports prompt-cache hits as input_tokens_details.cached_tokens,
          // a subset of input_tokens (the full prompt, kept for accounting).
          const cachedTokens = evt.response.usage.input_tokens_details?.cached_tokens ?? 0
          // Only computed when there's real per-field data to normalize — a thin usage report
          // (total_tokens only, no input_tokens/output_tokens) matches the same conditional-field
          // convention the two lines below already follow, rather than attaching a misleadingly
          // "complete" {inputTokens:0, outputTokens:0} object.
          const hasDetail = Boolean(evt.response.usage.input_tokens || evt.response.usage.output_tokens)
          const providerUsage = hasDetail ? normalizeOpenAIUsage(evt.response.usage) : undefined
          yield {
            type: "usage",
            totalTokens: evt.response.usage.total_tokens,
            ...(evt.response.usage.input_tokens ? { inputTokens: evt.response.usage.input_tokens } : {}),
            ...(evt.response.usage.output_tokens ? { outputTokens: evt.response.usage.output_tokens } : {}),
            ...(cachedTokens > 0 ? { cacheReadInputTokens: cachedTokens } : {}),
            ...(providerUsage ? { providerUsage } : {}),
          } as StreamEvent
        }
      }
    }
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, [
      "model", "input", "instructions", "tools", "stream", "previous_response_id",
      "web_search", "builtin_tools",
    ])
  }

  /** Responses API built-in server tools from extensions (live in the same tools[] as function tools):
   *  `web_search: true` (or a config object), plus a `builtin_tools` list passed through verbatim for
   *  file_search / code_interpreter. They run server-side; results return inline. Mirrors py. */
  private builtinTools(extensions?: Record<string, unknown>): Record<string, unknown>[] {
    const ext = extensions ?? {}
    const out: Record<string, unknown>[] = []
    const ws = ext.web_search
    if (ws) out.push(typeof ws === "object" ? { type: "web_search", ...ws } : { type: "web_search" })
    if (Array.isArray(ext.builtin_tools)) out.push(...(ext.builtin_tools as Record<string, unknown>[]))
    return out
  }

  /** Function tools + built-in server tools merged into the wire tools[] (undefined when empty). */
  private allTools(tools: ToolSchema[], extensions?: Record<string, unknown>): OpenAI.Responses.Tool[] | undefined {
    const fnTools = tools.length ? (this.responses.buildTools(tools) as OpenAI.Responses.Tool[]) : []
    const all = [...fnTools, ...this.builtinTools(extensions)] as OpenAI.Responses.Tool[]
    return all.length ? all : undefined
  }

  private asRunState(state?: ProviderRunState): OpenAIResponsesRunState {
    if (!state) return this.createRunState()
    if (typeof state.coveredMessageCount !== "number") state.coveredMessageCount = 0
    return state as OpenAIResponsesRunState
  }
}
