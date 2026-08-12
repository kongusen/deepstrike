import type OpenAI from "openai"
import type {
  Message,
  ProviderDescriptor,
  ProviderReplay,
  ProviderUsage,
  RenderedContext,
  StreamEvent,
  TextDelta,
  ThinkingDelta,
  ToolCall,
  ToolCallEvent,
  ToolSchema,
  UsageEvent,
} from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import {
  openAICachedPromptTokens,
  stablePromptCacheKey,
  ThinkingTagStreamExtractor,
} from "./base.js"
import type {
  CanonicalAdapterInput,
  CanonicalMessage,
} from "./content-normalization.js"
import { normalizeCanonicalContext, projectToolOutputToText } from "./content-normalization.js"
import { normalizeToolCall } from "./base.js"
import {
  DEGRADED_REASONING_PLACEHOLDER,
  assessReasoningReplay,
  validateOpenAIChatReplay,
  type ReplayabilityAssessment,
} from "./replay-validator.js"
import {
  type AdapterDecodeInput,
  type AdapterOutput,
  type AdapterStreamInput,
  type CanonicalStopReason,
  type ProtocolAdapter,
  ProtocolResponseError,
} from "./protocol-adapter.js"
import { OPENAI_CHAT_PROTOCOL_CAPABILITIES } from "./protocol-capabilities.js"
import {
  openAIChatDialects,
  replayForTurn,
  type OpenAIChatTurnReasoning,
  type OpenAIChatWireDialect,
} from "./openai-chat-dialects.js"

export interface OpenAIChatBuildMessageOptions {
  descriptor?: ProviderDescriptor
  requireNonEmptyReasoningForToolCalls?: boolean
  degradeMissingReasoning?: boolean
}

export interface OpenAIChatRequestPlan {
  params: Record<string, unknown>
  preparedExtensions: Record<string, unknown>
  dialect: OpenAIChatWireDialect
}

export type OpenAIChatStreamChunk = Record<string, any>

export interface OpenAIChatStreamState {
  readonly input: CanonicalAdapterInput
  readonly dialect: OpenAIChatWireDialect
  readonly toolCallBuffers: Record<number, { id: string; name: string; argsBuffer: string }>
  readonly emittedToolCallIndexes: Set<number>
  readonly extractor: ThinkingTagStreamExtractor
  accumulatedReasoning: string
  accumulatedReasoningDetails?: unknown
  accumulatedContent: string
  totalTokens: number
  inputTokens: number
  outputTokens: number
  cacheReadTokens: number
  finishReason?: string
  rawUsage?: unknown
}

const COMPATIBILITY_REPLAY = new WeakMap<OpenAIChatAdapter, Map<string, ProviderReplay>>()

function compatibilityReplayStore(adapter: OpenAIChatAdapter): Map<string, ProviderReplay> {
  let store = COMPATIBILITY_REPLAY.get(adapter)
  if (!store) {
    store = new Map()
    COMPATIBILITY_REPLAY.set(adapter, store)
  }
  return store
}

function wireReplay(replay: ProviderReplay | undefined): Record<string, unknown> | undefined {
  if (!replay) return undefined
  const fields: Record<string, unknown> = {}
  if (typeof replay.reasoning_content === "string") fields.reasoning_content = replay.reasoning_content
  if (replay.reasoning_details !== undefined) fields.reasoning_details = replay.reasoning_details
  return Object.keys(fields).length ? fields : undefined
}

function blockContent(message: CanonicalMessage): string | Array<Record<string, unknown>> {
  const content: Array<Record<string, unknown>> = []
  for (const block of message.blocks) {
    if (block.type === "tool_result") continue
    if (block.type === "text") content.push({ type: "text", text: block.text })
    else if (block.type === "image") {
      const url = block.source.kind === "url"
        ? block.source.url
        : block.source.kind === "base64"
          ? `data:${block.mediaType ?? "image/png"};base64,${block.source.data}`
          : undefined
      if (url) content.push({
        type: "image_url",
        image_url: {
          url,
          ...(block.providerOptions?.openai_detail
            ? { detail: block.providerOptions.openai_detail }
            : {}),
        },
      })
    } else if (block.type === "audio" && block.source.kind === "base64") {
      const subtype = (block.mediaType ?? "audio/wav").split("/")[1] ?? "wav"
      content.push({
        type: "input_audio",
        input_audio: {
          data: block.source.data,
          format: subtype === "mpeg" ? "mp3" : subtype,
        },
      })
    }
  }
  return message.contentForm !== "blocks" && content.length === 1 && content[0].type === "text"
    ? String(content[0].text ?? "")
    : content
}

function validateCanonicalReplay(
  input: CanonicalAdapterInput,
  dialect: OpenAIChatWireDialect,
  prepared: Readonly<Record<string, unknown>>,
): void {
  let pending: Set<string> | undefined
  let completed = new Set<string>()
  const assertComplete = () => {
    const missing = pending ? [...pending].filter(id => !completed.has(id)) : []
    if (missing.length) throw new Error(`OpenAI-compatible replay has assistant tool_calls with no tool result for ${missing.join(", ")}`)
  }
  for (const message of input.context.turns) {
    if (message.role === "assistant") {
      assertComplete()
      pending = message.toolCalls?.length ? new Set(message.toolCalls.map(call => call.id)) : undefined
      completed = new Set()
    } else if (message.role === "tool") {
      for (const block of message.blocks) {
        if (block.type !== "tool_result") continue
        if (!pending?.has(block.callId)) throw new Error(`OpenAI-compatible replay has orphan tool result ${block.callId}`)
        if (completed.has(block.callId)) throw new Error(`OpenAI-compatible replay has duplicate tool result ${block.callId}`)
        completed.add(block.callId)
      }
    } else {
      assertComplete()
      pending = undefined
      completed = new Set()
    }
  }
  assertComplete()

  if (dialect.requireReasoningReplay(prepared) && prepared.degradeMissingReasoningReplay !== true) {
    const missing = input.context.turns.flatMap(message =>
      message.role === "assistant" && message.toolCalls?.length
      && !(typeof message.providerReplay?.reasoning_content === "string" && message.providerReplay.reasoning_content.trim())
        ? message.toolCalls.map(call => call.id)
        : [],
    )
    if (missing.length) {
      throw new Error(`${dialect.providerId}/${input.resolved.identity.modelId} replay requires non-empty reasoning_content for assistant tool call turn ${missing.join(", ")}`)
    }
  }
}

function messages(
  input: CanonicalAdapterInput,
  dialect: OpenAIChatWireDialect,
  prepared: Readonly<Record<string, unknown>>,
): Array<Record<string, unknown>> {
  validateCanonicalReplay(input, dialect, prepared)
  const output: Array<Record<string, unknown>> = []
  if (input.context.systemText) output.push({ role: "system", content: input.context.systemText })
  const turns = input.context.stateTurn
    ? [...input.context.turns, input.context.stateTurn]
    : input.context.turns
  for (const message of turns) {
    if (message.role === "tool") {
      for (const block of message.blocks) {
        if (block.type !== "tool_result") continue
        output.push({
          role: "tool",
          tool_call_id: block.callId,
          content: projectToolOutputToText(block.blocks),
        })
      }
      continue
    }
    const next: Record<string, unknown> = { role: message.role, content: blockContent(message) }
    if (message.role === "assistant" && message.toolCalls?.length) {
      next.tool_calls = message.toolCalls.map(call => ({
        id: call.id,
        type: "function",
        function: { name: call.name, arguments: call.arguments },
      }))
      const replay = wireReplay(message.providerReplay)
        ?? (dialect.requireReasoningReplay(prepared) && prepared.degradeMissingReasoningReplay === true
          ? { reasoning_content: DEGRADED_REASONING_PLACEHOLDER }
          : undefined)
      if (replay) Object.assign(next, replay)
    }
    output.push(next)
  }
  return output
}

function nativeToolCalls(buffers: OpenAIChatStreamState["toolCallBuffers"]): unknown[] {
  return Object.values(buffers).map(call => ({
    id: call.id,
    type: "function",
    function: { name: call.name, arguments: call.argsBuffer || "{}" },
  }))
}

function finalToolCalls(state: OpenAIChatStreamState): ToolCall[] {
  return Object.values(state.toolCallBuffers).map(call => ({
    id: call.id,
    name: call.name,
    arguments: call.argsBuffer || "{}",
  }))
}

function streamReplay(state: OpenAIChatStreamState): ProviderReplay | undefined {
  return replayForTurn(
    state.dialect,
    "stream",
    state.input.resolved.identity.modelId,
    state.accumulatedContent,
    finalToolCalls(state),
    {
      reasoningContent: state.accumulatedReasoning,
      reasoningDetails: state.accumulatedReasoningDetails,
      nativeToolCalls: nativeToolCalls(state.toolCallBuffers),
    },
  )
}

function pendingToolEvents(state: OpenAIChatStreamState): ToolCallEvent[] {
  const events: ToolCallEvent[] = []
  for (const [rawIndex, call] of Object.entries(state.toolCallBuffers)) {
    const index = Number(rawIndex)
    if (state.emittedToolCallIndexes.has(index)) continue
    let args: Record<string, unknown> = {}
    try { args = JSON.parse(call.argsBuffer || "{}") as Record<string, unknown> } catch { args = {} }
    state.emittedToolCallIndexes.add(index)
    events.push({ type: "tool_call", id: call.id, name: call.name, arguments: args })
  }
  return events
}

function numberField(raw: Record<string, unknown>, key: string): number | undefined {
  const value = raw[key]
  if (value === undefined || value === null) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new ProtocolResponseError("openai-chat", `usage.${key} must be a non-negative finite number`)
  }
  return value
}

export class OpenAIChatAdapter implements ProtocolAdapter<
  OpenAIChatRequestPlan,
  Record<string, any>,
  OpenAIChatStreamChunk,
  OpenAIChatStreamState,
  undefined
> {
  // Responses are decoded from the official Chat Completions message/chunk shapes. The adapter
  // preserves pre-A-09 raw finish reasons; canonical stop mapping is exposed separately.
  // Source: https://platform.openai.com/docs/api-reference/chat/create
  readonly protocol = "openai-chat" as const
  readonly protocolCapabilities = OPENAI_CHAT_PROTOCOL_CAPABILITIES

  buildRequest(
    input: CanonicalAdapterInput,
    dialect: OpenAIChatWireDialect = openAIChatDialects.openai,
  ): OpenAIChatRequestPlan {
    const prepared = dialect.prepareExtensions(input.extensions)
    const tools = [
      ...this.buildTools(input.tools),
      ...(dialect.serverTools?.(input.extensions) ?? []),
    ]
    const cache = dialect.cacheKey === "openai"
      ? { prompt_cache_key: stablePromptCacheKey([
          input.context.systemText,
          input.tools.map(tool => tool.name).join(","),
        ]) }
      : {}
    return {
      dialect,
      preparedExtensions: prepared,
      params: {
        ...cache,
        ...Object.fromEntries(Object.entries(prepared).filter(([key]) =>
          key !== "__deepstrikeThinkingEnabled" && key !== "degradeMissingReasoningReplay")),
        model: input.resolved.identity.modelId,
        messages: messages(input, dialect, prepared),
        ...(tools.length ? { tools } : {}),
      },
    }
  }

  decodeComplete(
    raw: Record<string, any>,
    input: AdapterDecodeInput,
    dialect: OpenAIChatWireDialect = openAIChatDialects.openai,
  ): { message: Message; replay?: ProviderReplay } {
    const choice = raw.choices?.[0]?.message ?? {}
    const nativeCalls = choice.tool_calls ?? []
    const toolCalls = this.normalizeToolCalls(nativeCalls)
    const content = choice.content ?? ""
    const usage = raw.usage as Record<string, unknown> | undefined
    if (usage) this.normalizeUsage(usage)
    const message: Message = {
      role: "assistant",
      content,
      ...(usage ? {
        tokenCount: numberField(usage, "completion_tokens") ?? numberField(usage, "total_tokens"),
      } : {}),
      toolCalls,
    }
    const replay = replayForTurn(dialect, "complete", input.input.resolved.identity.modelId, content, toolCalls, {
      reasoningContent: typeof choice.reasoning_content === "string" ? choice.reasoning_content : "",
      reasoningDetails: choice.reasoning_details,
      nativeToolCalls: nativeCalls,
    })
    return { message, ...(replay ? { replay } : {}) }
  }

  createStreamState(
    input: AdapterStreamInput,
    dialect: OpenAIChatWireDialect = openAIChatDialects.openai,
  ): OpenAIChatStreamState {
    return {
      input: input.input,
      dialect,
      toolCallBuffers: {},
      emittedToolCallIndexes: new Set(),
      extractor: new ThinkingTagStreamExtractor(),
      accumulatedReasoning: "",
      accumulatedContent: "",
      totalTokens: 0,
      inputTokens: 0,
      outputTokens: 0,
      cacheReadTokens: 0,
    }
  }

  pushStreamChunk(chunk: OpenAIChatStreamChunk, state: OpenAIChatStreamState): AdapterOutput {
    if (chunk.usage) {
      state.totalTokens = chunk.usage.total_tokens
      state.inputTokens = chunk.usage.prompt_tokens ?? 0
      state.outputTokens = chunk.usage.completion_tokens ?? 0
      state.cacheReadTokens = openAICachedPromptTokens(chunk.usage)
      state.rawUsage = chunk.usage
      return { events: [] }
    }
    const choice = chunk.choices?.[0]
    if (!choice) return { events: [] }
    if (choice.finish_reason) state.finishReason = choice.finish_reason
    const delta = choice.delta as Record<string, any> | undefined
    if (!delta) return { events: [] }
    const events: StreamEvent[] = []
    if (delta.reasoning_content) {
      state.accumulatedReasoning += String(delta.reasoning_content)
      if (state.dialect.exposeReasoning(state.input.extensions)) {
        events.push({ type: "thinking_delta", delta: String(delta.reasoning_content) } as ThinkingDelta)
      }
    }
    if (delta.reasoning_details !== undefined && delta.reasoning_details !== null) {
      state.accumulatedReasoningDetails = delta.reasoning_details
    }
    if (delta.content) {
      if (state.dialect.inlineThinkingTags) {
        for (const part of state.extractor.feed(String(delta.content))) {
          if (part.type === "thinking") {
            state.accumulatedReasoning += part.content
            events.push({ type: "thinking_delta", delta: part.content } as ThinkingDelta)
          } else {
            state.accumulatedContent += part.content
            events.push({ type: "text_delta", delta: part.content } as TextDelta)
          }
        }
      } else {
        state.accumulatedContent += String(delta.content)
        events.push({ type: "text_delta", delta: String(delta.content) } as TextDelta)
      }
    }
    for (const call of delta.tool_calls ?? []) {
      const index = call.index
      if (!state.toolCallBuffers[index]) {
        state.toolCallBuffers[index] = { id: call.id ?? "", name: "", argsBuffer: "" }
      }
      if (call.function?.name) state.toolCallBuffers[index].name += call.function.name
      state.toolCallBuffers[index].argsBuffer += call.function?.arguments ?? ""
    }
    if (choice.finish_reason === "tool_calls") {
      events.push(...pendingToolEvents(state))
      const replay = streamReplay(state)
      return { events, ...(replay ? { replay } : {}) }
    }
    return { events }
  }

  finishStream(state: OpenAIChatStreamState): AdapterOutput {
    const events: StreamEvent[] = []
    if (state.dialect.inlineThinkingTags) {
      for (const part of state.extractor.flush()) {
        if (part.type === "thinking") {
          state.accumulatedReasoning += part.content
          events.push({ type: "thinking_delta", delta: part.content } as ThinkingDelta)
        } else {
          state.accumulatedContent += part.content
          events.push({ type: "text_delta", delta: part.content } as TextDelta)
        }
      }
    }
    events.push(...pendingToolEvents(state))
    if (state.totalTokens > 0) {
      const providerUsage = this.normalizeUsage(state.rawUsage)
      events.push({
        type: "usage",
        totalTokens: state.totalTokens,
        inputTokens: state.inputTokens,
        outputTokens: state.outputTokens,
        ...(state.cacheReadTokens > 0 ? { cacheReadInputTokens: state.cacheReadTokens } : {}),
        ...(state.finishReason ? { stopReason: state.finishReason } : {}),
        ...(providerUsage ? { providerUsage } : {}),
      } as UsageEvent)
    }
    const replay = streamReplay(state)
    return { events, ...(replay ? { replay } : {}) }
  }

  normalizeUsage(raw: unknown): ProviderUsage | undefined {
    if (raw === undefined || raw === null) return undefined
    if (typeof raw !== "object" || Array.isArray(raw)) {
      throw new ProtocolResponseError("openai-chat", "usage must be an object")
    }
    const usage = raw as Record<string, unknown>
    const inputTokens = numberField(usage, "prompt_tokens")
    const outputTokens = numberField(usage, "completion_tokens")
    numberField(usage, "total_tokens")
    if (inputTokens === undefined && outputTokens === undefined) return undefined
    const cacheReadInputTokens = openAICachedPromptTokens(usage)
    const details = usage.completion_tokens_details
    const reasoningTokens = details && typeof details === "object"
      ? numberField(details as Record<string, unknown>, "reasoning_tokens")
      : undefined
    return {
      inputTokens: inputTokens ?? 0,
      outputTokens: outputTokens ?? 0,
      ...(cacheReadInputTokens > 0 ? { cacheReadInputTokens } : {}),
      ...(reasoningTokens !== undefined ? { reasoningTokens } : {}),
    }
  }

  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined {
    if (raw === undefined) return undefined
    if (raw === "length") return "max_tokens"
    if (raw === "stop") return "end_turn"
    if (raw === "tool_calls") return "tool_use"
    if (raw === "content_filter") return "content_filter"
    return "other"
  }

  buildTools(tools: readonly ToolSchema[]) {
    return tools.map(tool => ({
      type: "function" as const,
      function: {
        name: tool.name,
        description: tool.description,
        parameters: JSON.parse(tool.parameters),
      },
    }))
  }

  buildMessages(
    context: RenderedContext,
    options: OpenAIChatBuildMessageOptions = {},
  ): OpenAI.ChatCompletionMessageParam[] {
    validateOpenAIChatReplay(context, {
      descriptor: options.descriptor,
      requireNonEmptyReasoningForToolCalls: options.requireNonEmptyReasoningForToolCalls,
      degradeMissingReasoning: options.degradeMissingReasoning,
      replayForAssistant: message => compatibilityReplayStore(this).get(assistantReplayKey(message)),
    })
    const canonical = normalizeCanonicalContext(
      context,
      message => compatibilityReplayStore(this).get(assistantReplayKey(message)),
    )
    const dialect: OpenAIChatWireDialect = {
      ...openAIChatDialects.openai,
      descriptor: { reasoning: options.descriptor?.reasoning ?? openAIChatDialects.openai.descriptor.reasoning },
      requireReasoningReplay: () => options.requireNonEmptyReasoningForToolCalls === true,
    }
    return messages({
      context: canonical,
      tools: [],
      resolved: { identity: { modelId: options.descriptor?.model ?? "compat" } } as CanonicalAdapterInput["resolved"],
      extensions: options.degradeMissingReasoning ? { degradeMissingReasoningReplay: true } : {},
    }, dialect, options.degradeMissingReasoning ? { degradeMissingReasoningReplay: true } : {}) as unknown as OpenAI.ChatCompletionMessageParam[]
  }

  assessReasoning(context: RenderedContext): ReplayabilityAssessment {
    return assessReasoningReplay(context.turns, {
      replayForAssistant: message => compatibilityReplayStore(this).get(assistantReplayKey(message)),
    })
  }

  normalizeToolCalls(toolCalls: OpenAI.ChatCompletionMessageToolCall[] = []): ToolCall[] {
    return toolCalls
      .filter((call): call is OpenAI.ChatCompletionMessageFunctionToolCall => call.type === "function")
      .map(call => normalizeToolCall(call.id, call.function.name, call.function.arguments))
      .filter((call): call is ToolCall => call !== null)
  }

  rememberReplayFields(message: Pick<Message, "content" | "toolCalls">, fields: Record<string, unknown>): void {
    compatibilityReplayStore(this).set(assistantReplayKey(message), fields as ProviderReplay)
  }

  peekReplayFields(message: Pick<Message, "content" | "toolCalls">): Record<string, unknown> | undefined {
    return compatibilityReplayStore(this).get(assistantReplayKey(message)) as Record<string, unknown> | undefined
  }
}
