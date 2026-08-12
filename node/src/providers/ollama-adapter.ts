import type {
  Message,
  ProviderUsage,
  StreamEvent,
  TextDelta,
  ToolCallEvent,
  UsageEvent,
} from "../types.js"
import type {
  CanonicalAdapterInput,
  CanonicalMessage,
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
  OLLAMA_PROTOCOL_CAPABILITIES,
} from "./protocol-adapter.js"

export interface OllamaMessage {
  role: string
  content: string
  images?: string[]
}

export interface OllamaRequest {
  model: string
  messages: OllamaMessage[]
  tools?: Array<{
    type: "function"
    function: { name: string; description: string; parameters: unknown }
  }>
  [key: string]: unknown
}

export interface OllamaChunk {
  message?: {
    content?: string
    tool_calls?: Array<{
      function: { name: string; arguments: unknown }
    }>
  }
  done?: boolean
  done_reason?: string
  prompt_eval_count?: number
  eval_count?: number
}

export interface OllamaStreamState {
  readonly input: CanonicalAdapterInput
  readonly pendingToolCalls: Map<string, {
    id: string
    name: string
    arguments: Record<string, unknown>
  }>
  finalChunk?: OllamaChunk
}

function messageContent(message: CanonicalMessage): OllamaMessage {
  const text: string[] = []
  const images: string[] = []
  for (const item of message.blocks) {
    if (item.type === "tool_result") {
      text.push(projectToolOutputToText(item.blocks))
    } else if (item.type === "text") {
      text.push(item.text)
    } else if (item.type === "image" && item.source.kind === "base64") {
      images.push(item.source.data)
      text.push("[image]")
    } else {
      throw new ProtocolResponseError("ollama-chat", `cannot serialize ${item.type}`)
    }
  }
  return {
    role: message.role,
    content: text.join("\n"),
    ...(images.length ? { images } : {}),
  }
}

function requestExtensions(
  extensions: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const blocked = new Set([
    "model",
    "messages",
    "tools",
    "stream",
    "__deepstrikeThinkingEnabled",
    "degradeMissingReasoningReplay",
  ])
  return Object.fromEntries(
    Object.entries(extensions).filter(([key]) => !blocked.has(key)),
  )
}

function validCount(raw: Record<string, unknown>, field: string): number | undefined {
  const value = raw[field]
  if (value === undefined) return undefined
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new ProtocolResponseError(
      "ollama-chat",
      `${field} must be a non-negative finite number`,
    )
  }
  return value
}

export class OllamaNdjsonDecoder {
  private buffer = ""

  push(text: string): OllamaChunk[] {
    this.buffer += text
    const lines = this.buffer.split("\n")
    this.buffer = lines.pop() ?? ""
    return this.parse(lines)
  }

  finish(text = ""): OllamaChunk[] {
    this.buffer += text
    const tail = this.buffer
    this.buffer = ""
    return this.parse(tail ? [tail] : [])
  }

  private parse(lines: readonly string[]): OllamaChunk[] {
    const chunks: OllamaChunk[] = []
    for (const line of lines) {
      if (!line.trim()) continue
      try {
        const value = JSON.parse(line) as unknown
        if (value && typeof value === "object" && !Array.isArray(value)) {
          chunks.push(value as OllamaChunk)
        }
      } catch {
        // Preserve the established Ollama behavior: malformed complete lines are skipped.
      }
    }
    return chunks
  }
}

export class OllamaAdapter implements ProtocolAdapter<
  OllamaRequest,
  OllamaChunk,
  OllamaChunk,
  OllamaStreamState,
  OllamaChunk | undefined
> {
  readonly protocol = "ollama-chat" as const
  readonly protocolCapabilities = OLLAMA_PROTOCOL_CAPABILITIES

  buildRequest(input: CanonicalAdapterInput): OllamaRequest {
    const messages: OllamaMessage[] = []
    if (input.context.systemText) {
      messages.push({ role: "system", content: input.context.systemText })
    }
    const turns = input.context.stateTurn
      ? [...input.context.turns, input.context.stateTurn]
      : input.context.turns
    messages.push(...turns.map(messageContent))
    return {
      ...requestExtensions(input.extensions),
      model: input.resolved.identity.modelId,
      messages,
      ...(input.tools.length ? {
        tools: input.tools.map(tool => ({
          type: "function" as const,
          function: {
            name: tool.name,
            description: tool.description,
            parameters: JSON.parse(tool.parameters) as unknown,
          },
        })),
      } : {}),
    }
  }

  decodeComplete(raw: OllamaChunk, _input: AdapterDecodeInput): { message: Message } {
    return {
      message: {
        role: "assistant",
        content: raw.message?.content ?? "",
      },
    }
  }

  createStreamState(input: AdapterStreamInput): OllamaStreamState {
    return { input: input.input, pendingToolCalls: new Map() }
  }

  pushStreamChunk(chunk: OllamaChunk, state: OllamaStreamState): AdapterOutput {
    const events: StreamEvent[] = []
    if (chunk.message?.content) {
      events.push({ type: "text_delta", delta: chunk.message.content } as TextDelta)
    }
    for (const call of chunk.message?.tool_calls ?? []) {
      const normalized = normalizeToolCall(
        "",
        call.function.name,
        call.function.arguments,
      )
      if (!normalized) continue
      const key = `${normalized.name}:${normalized.arguments}`
      if (!state.pendingToolCalls.has(key)) {
        state.pendingToolCalls.set(key, {
          id: `call_${state.pendingToolCalls.size + 1}`,
          name: normalized.name,
          arguments: JSON.parse(normalized.arguments) as Record<string, unknown>,
        })
      }
    }
    if (chunk.done) state.finalChunk = chunk
    return { events }
  }

  finishStream(
    state: OllamaStreamState,
    final: OllamaChunk | undefined,
  ): AdapterOutput {
    const terminal = final ?? state.finalChunk
    const events: StreamEvent[] = Array.from(
      state.pendingToolCalls.values(),
      call => ({ type: "tool_call", ...call }) as ToolCallEvent,
    )
    const usage = this.normalizeUsage(terminal)
    if (usage) {
      const rawStopReason = terminal?.done_reason
      const stopReason = this.normalizeStopReason(rawStopReason)
      events.push({
        type: "usage",
        totalTokens: usage.inputTokens + usage.outputTokens,
        inputTokens: usage.inputTokens,
        outputTokens: usage.outputTokens,
        providerUsage: usage,
        ...(stopReason ? { stopReason } : {}),
        ...(rawStopReason ? { rawStopReason } : {}),
      } as UsageEvent)
    }
    return { events }
  }

  normalizeUsage(raw: unknown): ProviderUsage | undefined {
    if (raw === undefined || raw === null) return undefined
    if (typeof raw !== "object" || Array.isArray(raw)) {
      throw new ProtocolResponseError("ollama-chat", "usage source must be an object")
    }
    const record = raw as Record<string, unknown>
    const inputTokens = validCount(record, "prompt_eval_count")
    const outputTokens = validCount(record, "eval_count")
    if (inputTokens === undefined && outputTokens === undefined) return undefined
    return {
      inputTokens: inputTokens ?? 0,
      outputTokens: outputTokens ?? 0,
    }
  }

  normalizeStopReason(raw: string | undefined): CanonicalStopReason | undefined {
    if (raw === undefined) return undefined
    switch (raw) {
      case "stop": return "end_turn"
      case "length": return "max_tokens"
      default: return "other"
    }
  }

  createNdjsonDecoder(): OllamaNdjsonDecoder {
    return new OllamaNdjsonDecoder()
  }
}
