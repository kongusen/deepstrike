import type {
  CacheBreakpointStrategy,
  Message,
  ProviderReplay,
  ProviderUsage,
  StreamEvent,
  ThinkingDelta,
  ToolCall,
  ToolCallEvent,
  UsageEvent,
} from "../types.js"
import type {
  CanonicalAdapterInput,
  CanonicalMessage,
  CanonicalToolResult,
} from "./content-normalization.js"
import { projectToolOutputToText } from "./content-normalization.js"
import { normalizeToolCall } from "./base.js"
import {
  type AdapterDecodeInput,
  type AdapterOutput,
  type AdapterStreamInput,
  type CanonicalStopReason,
  type ProtocolAdapter,
  ProtocolResponseError,
} from "./protocol-adapter.js"
import { ANTHROPIC_PROTOCOL_CAPABILITIES } from "./protocol-capabilities.js"

export interface AnthropicRequestPlan {
  transport: "stable" | "beta"
  params: Record<string, unknown>
  cacheSlots: { system: boolean; tools: boolean; messages: boolean }
}

export type AnthropicStreamChunk = Record<string, any>

export interface AnthropicStreamState {
  readonly input: CanonicalAdapterInput
  readonly cacheSlots: { system: boolean; tools: boolean; messages: boolean }
  readonly toolBlocks: Record<number, { id: string; name: string; argsBuffer: string }>
  readonly nativeBlocks: Record<number, Record<string, unknown>>
  readonly finalToolCalls: ToolCall[]
  finalText: string
  uncachedInput: number
  cacheReadTokens: number
  cacheCreationTokens: number
  outputTokens: number
}

const CACHE_BREAKPOINT_STRATEGIES = new Set<CacheBreakpointStrategy>([
  "default", "tools-only", "system-only", "frozen-prefix", "none",
])

function cacheStrategy(extensions: Readonly<Record<string, unknown>>): CacheBreakpointStrategy {
  const raw = extensions.cacheBreakpointStrategy
  return typeof raw === "string" && CACHE_BREAKPOINT_STRATEGIES.has(raw as CacheBreakpointStrategy)
    ? raw as CacheBreakpointStrategy
    : "default"
}

function extensionsForWire(
  extensions: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const blocked = new Set([
    "model", "messages", "system", "tools", "max_tokens", "stream",
    "__deepstrikeThinkingEnabled", "degradeMissingReasoningReplay",
  ])
  return Object.fromEntries(Object.entries(extensions).filter(([key]) => !blocked.has(key)))
}

function systemBlocks(
  context: CanonicalAdapterInput["context"],
  strategy: CacheBreakpointStrategy,
): string | Array<Record<string, unknown>> | undefined {
  if (!context.systemStable && !context.systemKnowledge) {
    return context.systemText || undefined
  }
  const cache = strategy === "default" || strategy === "system-only"
  const blocks: Array<Record<string, unknown>> = []
  if (context.systemStable) {
    blocks.push({
      type: "text",
      text: context.systemStable,
      ...(cache ? { cache_control: { type: "ephemeral" } } : {}),
    })
  }
  if (context.systemKnowledge) {
    blocks.push({
      type: "text",
      text: context.systemKnowledge,
      ...(cache ? { cache_control: { type: "ephemeral" } } : {}),
    })
  }
  return blocks.length ? blocks : undefined
}

function toolResult(result: CanonicalToolResult): Record<string, unknown> {
  const blocks = result.blocks
  const native = blocks.map(block => {
    if (block.type === "text") return { type: "text", text: block.text }
    if (block.type === "image") {
      if (block.source.kind === "base64") {
        return {
          type: "image",
          source: {
            type: "base64",
            media_type: block.mediaType ?? "image/png",
            data: block.source.data,
          },
        }
      }
      if (block.source.kind === "url") {
        return { type: "image", source: { type: "url", url: block.source.url } }
      }
    }
    return { type: "text", text: `[${block.type}]` }
  })
  return {
    type: "tool_result",
    tool_use_id: result.callId,
    content: result.contentForm !== "blocks" && blocks.length === 1 && blocks[0].type === "text"
      ? blocks[0].text
      : native,
    is_error: result.isError,
  }
}

function canonicalMessage(message: CanonicalMessage): Record<string, unknown> | undefined {
  if (
    message.role === "assistant"
    && message.toolCalls?.length
    && message.providerReplay?.native_blocks?.length
  ) {
    return { role: "assistant", content: ensureAssistantToolText(message.providerReplay.native_blocks) }
  }
  const content: Array<Record<string, unknown>> = []
  for (const item of message.blocks) {
    if (item.type === "tool_result") {
      content.push(toolResult(item))
    } else if (item.type === "text") {
      if (item.text) content.push({ type: "text", text: item.text })
    } else if (item.type === "image") {
      if (item.source.kind === "base64") {
        content.push({
          type: "image",
          source: {
            type: "base64",
            media_type: item.mediaType ?? "image/png",
            data: item.source.data,
          },
        })
      } else if (item.source.kind === "url") {
        content.push({ type: "image", source: { type: "url", url: item.source.url } })
      }
    }
  }
  if (message.role === "assistant" && message.toolCalls?.length) {
    if (!content.length) content.push({ type: "text", text: "Tool call requested." })
    for (const call of message.toolCalls) {
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(call.arguments) as Record<string, unknown> } catch { args = {} }
      content.push({ type: "tool_use", id: call.id, name: call.name, input: args })
    }
  }
  if (message.role === "tool" && !content.some(block => block.type === "tool_result")) {
    return undefined
  }
  return {
    role: message.role === "tool" ? "user" : message.role,
    content: message.contentForm !== "blocks" && content.length === 1 && content[0].type === "text"
      ? content[0].text
      : content,
  }
}

function ensureAssistantToolText(
  blocks: Array<Record<string, unknown>>,
): Array<Record<string, unknown>> {
  if (!blocks.some(block => block.type === "tool_use")) return blocks
  if (blocks.some(block => block.type === "text" && String(block.text ?? "").trim())) return blocks
  if (blocks.some(block => block.type === "thinking")) return blocks
  return [{ type: "text", text: "Tool call requested." }, ...blocks]
}

function markLastBlockCacheable(message: Record<string, unknown>): void {
  const cache_control = { type: "ephemeral" }
  if (typeof message.content === "string") {
    if (message.content) {
      message.content = [{ type: "text", text: message.content, cache_control }]
    }
    return
  }
  if (Array.isArray(message.content) && message.content.length) {
    const last = message.content.at(-1) as Record<string, unknown>
    last.cache_control = cache_control
  }
}

function applyMessageCacheControl(
  messages: Array<Record<string, unknown>>,
  frozenPrefixLen: number | undefined,
  strategy: CacheBreakpointStrategy,
): void {
  if (!messages.length || ["tools-only", "system-only", "none"].includes(strategy)) return
  const targets = new Set([messages.length - 1])
  if (typeof frozenPrefixLen === "number" && frozenPrefixLen >= 1 && frozenPrefixLen < messages.length) {
    targets.add(frozenPrefixLen - 1)
  } else if (strategy === "default") {
    for (let index = messages.length - 2; index >= 0 && targets.size < 2; index--) {
      if (messages[index].role === "user") targets.add(index)
    }
  }
  for (const index of targets) markLastBlockCacheable(messages[index])
}

function requestTools(
  input: CanonicalAdapterInput,
  system: string | Array<Record<string, unknown>> | undefined,
  strategy: CacheBreakpointStrategy,
): Array<Record<string, unknown>> | undefined {
  if (!input.tools.length) return undefined
  const anchor = !Array.isArray(system)
    && (strategy === "default" || strategy === "tools-only")
  return input.tools.map((tool, index) => ({
    name: tool.name,
    description: tool.description,
    input_schema: JSON.parse(tool.parameters),
    ...(anchor && index === input.tools.length - 1
      ? { cache_control: { type: "ephemeral" } }
      : {}),
  }))
}

function countSlots(
  system: string | Array<Record<string, unknown>> | undefined,
  tools: Array<Record<string, unknown>> | undefined,
  messages: Array<Record<string, unknown>>,
): AnthropicRequestPlan["cacheSlots"] {
  return {
    system: Array.isArray(system) && system.some(block => block.cache_control),
    tools: !!tools?.some(tool => tool.cache_control),
    messages: messages.some(message =>
      Array.isArray(message.content)
      && (message.content as Array<Record<string, unknown>>).some(block => block.cache_control)),
  }
}

function countCacheBreakpoints(value: unknown): number {
  if (Array.isArray(value)) return value.reduce((total, item) => total + countCacheBreakpoints(item), 0)
  if (!value || typeof value !== "object") return 0
  const record = value as Record<string, unknown>
  return (record.cache_control ? 1 : 0)
    + Object.entries(record)
      .filter(([key]) => key !== "cache_control")
      .reduce((total, [, item]) => total + countCacheBreakpoints(item), 0)
}

function assertCacheBudget(params: Record<string, unknown>): void {
  const count = countCacheBreakpoints({
    system: params.system,
    tools: params.tools,
    messages: params.messages,
  })
  if (count > 4) {
    throw new Error(`Anthropic cache_control budget exceeded: ${count} > 4`)
  }
}

function estimateCacheRead(
  cacheRead: number,
  slots: AnthropicRequestPlan["cacheSlots"],
): { system?: number; tools?: number; messages?: number } | undefined {
  if (cacheRead <= 0) return undefined
  const keys = (["system", "tools", "messages"] as const).filter(key => slots[key])
  if (!keys.length) return undefined
  const share = Math.floor(cacheRead / keys.length)
  const remainder = cacheRead - share * keys.length
  return Object.fromEntries(keys.map((key, index) => [key, share + (index === 0 ? remainder : 0)]))
}

function numeric(raw: Record<string, unknown>, field: string): number | undefined {
  const value = raw[field]
  if (value === undefined || value === null) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new ProtocolResponseError("anthropic-messages", `usage.${field} is invalid`)
  }
  return value
}

export class AnthropicMessagesAdapter implements ProtocolAdapter<
  AnthropicRequestPlan,
  Record<string, any>,
  AnthropicStreamChunk,
  AnthropicStreamState,
  undefined
> {
  readonly protocol = "anthropic-messages" as const
  readonly protocolCapabilities = ANTHROPIC_PROTOCOL_CAPABILITIES

  buildRequest(input: CanonicalAdapterInput): AnthropicRequestPlan {
    const strategy = cacheStrategy(input.extensions)
    const system = systemBlocks(input.context, strategy)
    const messages = input.context.turns
      .map(canonicalMessage)
      .filter((message): message is Record<string, unknown> => message !== undefined)
    applyMessageCacheControl(messages, input.context.frozenPrefixLen, strategy)
    if (input.context.stateTurn) {
      const stateMessage = canonicalMessage(input.context.stateTurn)
      if (stateMessage) messages.push(stateMessage)
    }
    if (!messages.length) messages.push({ role: "user", content: "Proceed." })
    const tools = requestTools(input, system, strategy)
    const wire = extensionsForWire(input.extensions)
    const betas = Array.isArray(input.extensions.betas) && input.extensions.betas.length
      ? input.extensions.betas
      : undefined
    const params = {
      ...wire,
      model: input.resolved.identity.modelId,
      max_tokens: typeof input.extensions.max_tokens === "number"
        ? input.extensions.max_tokens
        : 8096,
      ...(system ? { system } : {}),
      messages,
      ...(tools ? { tools } : {}),
      ...(betas ? { betas } : {}),
    }
    assertCacheBudget(params)
    return {
      transport: betas ? "beta" : "stable",
      params,
      cacheSlots: countSlots(system, tools, messages),
    }
  }

  decodeComplete(raw: Record<string, any>, _input: AdapterDecodeInput): {
    message: Message
    replay?: ProviderReplay
  } {
    let content = ""
    const toolCalls: ToolCall[] = []
    for (const block of raw.content ?? []) {
      if (block.type === "text") content += block.text
      else if (block.type === "tool_use") {
        const call = normalizeToolCall(block.id, block.name, block.input)
        if (call) toolCalls.push(call)
      }
    }
    const usage = this.normalizeUsage(raw.usage)
    const blocks = raw.content as Array<Record<string, unknown>> | undefined
    return {
      message: {
        role: "assistant",
        content,
        ...(usage ? { tokenCount: usage.outputTokens } : {}),
        toolCalls,
      },
      ...(blocks?.length ? { replay: { native_blocks: blocks } } : {}),
    }
  }

  createStreamState(input: AdapterStreamInput): AnthropicStreamState {
    const plan = this.buildRequest(input.input)
    return {
      input: input.input,
      cacheSlots: plan.cacheSlots,
      toolBlocks: {},
      nativeBlocks: {},
      finalToolCalls: [],
      finalText: "",
      uncachedInput: 0,
      cacheReadTokens: 0,
      cacheCreationTokens: 0,
      outputTokens: 0,
    }
  }

  pushStreamChunk(chunk: AnthropicStreamChunk, state: AnthropicStreamState): AdapterOutput {
    const events: StreamEvent[] = []
    if (chunk.type === "message_start" || chunk.type === "message_delta") {
      const raw = chunk.usage ?? chunk.message?.usage
      if (raw) {
        state.uncachedInput = Math.max(state.uncachedInput, numeric(raw, "input_tokens") ?? 0)
        state.cacheReadTokens = Math.max(state.cacheReadTokens, numeric(raw, "cache_read_input_tokens") ?? 0)
        state.cacheCreationTokens = Math.max(state.cacheCreationTokens, numeric(raw, "cache_creation_input_tokens") ?? 0)
        state.outputTokens = Math.max(state.outputTokens, numeric(raw, "output_tokens") ?? 0)
        const inputTokens = state.uncachedInput + state.cacheReadTokens + state.cacheCreationTokens
        const providerUsage: ProviderUsage = {
          inputTokens,
          outputTokens: state.outputTokens,
          ...(state.cacheReadTokens ? { cacheReadInputTokens: state.cacheReadTokens } : {}),
          ...(state.cacheCreationTokens ? { cacheCreationInputTokens: state.cacheCreationTokens } : {}),
        }
        const rawStopReason = chunk.delta?.stop_reason as string | undefined
        const stopReason = this.normalizeStopReason(rawStopReason)
        const bySlot = estimateCacheRead(state.cacheReadTokens, state.cacheSlots)
        events.push({
          type: "usage",
          totalTokens: inputTokens + state.outputTokens,
          inputTokens,
          outputTokens: state.outputTokens,
          cacheReadInputTokens: state.cacheReadTokens,
          cacheCreationInputTokens: state.cacheCreationTokens,
          ...(bySlot ? { cacheReadInputTokensBySlot: bySlot } : {}),
          ...(stopReason ? { stopReason } : {}),
          ...(rawStopReason ? { rawStopReason } : {}),
          providerUsage,
        } as UsageEvent)
      }
    } else if (chunk.type === "content_block_start") {
      state.nativeBlocks[chunk.index] = { ...chunk.content_block }
      if (chunk.content_block.type === "tool_use") {
        state.toolBlocks[chunk.index] = {
          id: chunk.content_block.id,
          name: chunk.content_block.name,
          argsBuffer: "",
        }
      }
    } else if (chunk.type === "content_block_delta") {
      const delta = chunk.delta
      if (delta.type === "text_delta") {
        state.finalText += delta.text
        state.nativeBlocks[chunk.index] = {
          ...state.nativeBlocks[chunk.index],
          text: String(state.nativeBlocks[chunk.index]?.text ?? "") + delta.text,
        }
        events.push({ type: "text_delta", delta: delta.text } as StreamEvent)
      } else if (delta.type === "thinking_delta") {
        state.nativeBlocks[chunk.index] = {
          ...state.nativeBlocks[chunk.index],
          thinking: String(state.nativeBlocks[chunk.index]?.thinking ?? "") + delta.thinking,
        }
        events.push({ type: "thinking_delta", delta: delta.thinking } as ThinkingDelta)
      } else if (delta.type === "signature_delta") {
        state.nativeBlocks[chunk.index] = {
          ...state.nativeBlocks[chunk.index],
          signature: String(state.nativeBlocks[chunk.index]?.signature ?? "") + delta.signature,
        }
      } else if (delta.type === "input_json_delta" && state.toolBlocks[chunk.index]) {
        state.toolBlocks[chunk.index].argsBuffer += delta.partial_json
      }
    } else if (chunk.type === "content_block_stop" && state.toolBlocks[chunk.index]) {
      const tool = state.toolBlocks[chunk.index]
      delete state.toolBlocks[chunk.index]
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(tool.argsBuffer || "{}") as Record<string, unknown> } catch { args = {} }
      state.nativeBlocks[chunk.index] = { ...state.nativeBlocks[chunk.index], input: args }
      state.finalToolCalls.push({
        id: tool.id,
        name: tool.name,
        arguments: JSON.stringify(args),
      })
      events.push({
        type: "tool_call",
        id: tool.id,
        name: tool.name,
        arguments: args,
      } as ToolCallEvent)
    }
    return { events }
  }

  finishStream(state: AnthropicStreamState): AdapterOutput {
    const blocks = Object.keys(state.nativeBlocks)
      .map(Number)
      .sort((left, right) => left - right)
      .map(index => state.nativeBlocks[index])
    return {
      events: [],
      ...(blocks.length ? { replay: { native_blocks: blocks } } : {}),
    }
  }

  normalizeUsage(raw: unknown): ProviderUsage | undefined {
    if (raw === undefined || raw === null) return undefined
    if (typeof raw !== "object" || Array.isArray(raw)) {
      throw new ProtocolResponseError("anthropic-messages", "usage must be an object")
    }
    const record = raw as Record<string, unknown>
    const uncached = numeric(record, "input_tokens")
    const cacheRead = numeric(record, "cache_read_input_tokens")
    const cacheCreation = numeric(record, "cache_creation_input_tokens")
    const output = numeric(record, "output_tokens")
    if (
      uncached === undefined && cacheRead === undefined
      && cacheCreation === undefined && output === undefined
    ) return undefined
    return {
      inputTokens: (uncached ?? 0) + (cacheRead ?? 0) + (cacheCreation ?? 0),
      outputTokens: output ?? 0,
      ...(cacheRead ? { cacheReadInputTokens: cacheRead } : {}),
      ...(cacheCreation ? { cacheCreationInputTokens: cacheCreation } : {}),
    }
  }

  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined {
    if (raw === undefined) return undefined
    switch (raw) {
      case "end_turn": return "end_turn"
      case "tool_use": return "tool_use"
      case "max_tokens": return "max_tokens"
      case "stop_sequence": return "stop_sequence"
      default: return "other"
    }
  }
}
