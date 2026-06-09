import OpenAI from "openai"
import type { Message, ProviderDescriptor, RenderedContext, RuntimePolicy, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, ToolSchema } from "../types.js"
import { AnthropicProvider } from "./anthropic.js"
import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"
import { omitExtensionKeys } from "./base.js"

const MINIMAX_POLICIES: Record<string, RuntimePolicy> = {
  "MiniMax-M2.7":           { maxTurns: 35 },
  "MiniMax-M2.7-highspeed": { maxTurns: 35 },
  "MiniMax-M2.5":           { maxTurns: 25 },
  "MiniMax-M2.5-highspeed": { maxTurns: 25 },
  "MiniMax-M2.1":           { maxTurns: 25 },
  "MiniMax-M2.1-highspeed": { maxTurns: 25 },
  "MiniMax-M2":             { maxTurns: 20 },
  "MiniMax-Text-01":        { maxTurns: 20 },
}

/**
 * MiniMax over its Anthropic-compatible endpoint. Replay is carried as Anthropic
 * `native_blocks` (thinking/text/tool_use), identical to the first-party
 * Anthropic provider.
 */
export class MiniMaxAnthropicProvider extends AnthropicProvider {
  constructor(
    apiKey: string,
    model: string = "MiniMax-M2.7",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["minimax.anthropic"].baseURL,
  ) {
    super(apiKey, model, retry, {
      baseURL,
      authMode: "api-key",
    })
  }

  protected override providerName(): string {
    return "minimax"
  }

  override runtimePolicy(): RuntimePolicy {
    return MINIMAX_POLICIES[this.model] ?? {}
  }
}

/**
 * MiniMax over its OpenAI-compatible endpoint. Replay is carried as
 * `reasoning_content` / `reasoning_details` (split reasoning), and requests
 * default to `reasoning_split: true` so reasoning is returned out-of-band rather
 * than embedded in the message content.
 */
export class MiniMaxOpenAIProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "MiniMax-M2.7",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["minimax.openai"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return MINIMAX_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      provider: "minimax",
      protocol: "openai-chat",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: true,
        requiresReplayForToolTurns: true,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
    }
  }

  protected override requireNonEmptyReasoningReplayForToolTurns(extensions?: Record<string, unknown>): boolean {
    if (extensions?.__deepstrikeThinkingEnabled === false) return false
    return extensions?.reasoning_split !== false
  }

  private buildRequestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    const reasoningSplit = extensions?.reasoning_split !== false
    return {
      ...omitExtensionKeys(extensions, ["reasoning_split", "exposeReasoning"]),
      __deepstrikeThinkingEnabled: reasoningSplit,
      reasoning_split: reasoningSplit,
    }
  }

  override async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    const requestExtensions = this.buildRequestExtensions(extensions)
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = this.buildChatMessages(context, requestExtensions)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          ...this.requestExtensions(requestExtensions),
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message as OpenAI.ChatCompletionMessage & Record<string, unknown>
        const nativeToolCalls = choice.tool_calls ?? []
        const toolCalls = this.chat.normalizeToolCalls(nativeToolCalls)
        const content = choice.content ?? ""
        this.rememberMiniMaxReplay(content, toolCalls, choice.reasoning_content, choice.reasoning_details, nativeToolCalls)
        return { role: "assistant", content, tokenCount: resp.usage?.completion_tokens ?? resp.usage?.total_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  override async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = extensions?.exposeReasoning ?? false
    const requestExtensions = this.buildRequestExtensions(extensions)
    const msgs = this.buildChatMessages(context, requestExtensions)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const emittedToolCallIndexes = new Set<number>()
    let reasoningContent = ""
    let reasoningDetails: unknown
    let finalText = ""

    const stream = await this.client.chat.completions.create({
      ...this.requestExtensions(requestExtensions),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    let totalTokens = 0
    let inputTokens = 0
    let outputTokens = 0
    for await (const chunk of stream) {
      if (chunk.usage) {
        totalTokens = chunk.usage.total_tokens
        inputTokens = chunk.usage.prompt_tokens ?? 0
        outputTokens = chunk.usage.completion_tokens ?? 0
        continue
      }
      const choice = chunk.choices[0]
      if (!choice) continue
      const delta = choice.delta as Record<string, unknown>
      if (!delta) continue
      if (exposeReasoning && delta.reasoning_content) {
        yield { type: "thinking_delta", delta: String(delta.reasoning_content) } as ThinkingDelta
      }
      if (delta.reasoning_content) reasoningContent += String(delta.reasoning_content)
      if (delta.reasoning_details !== undefined && delta.reasoning_details !== null) reasoningDetails = delta.reasoning_details
      if (delta.content) {
        finalText += String(delta.content)
        yield { type: "text_delta", delta: delta.content } as TextDelta
      }
      for (const tc of (delta.tool_calls as OpenAI.ChatCompletionChunk.Choice.Delta.ToolCall[] | undefined) ?? []) {
        const idx = tc.index
        if (!toolCallBufs[idx]) toolCallBufs[idx] = { id: tc.id ?? "", name: "", argsBuf: "" }
        if (tc.function?.name) toolCallBufs[idx].name += tc.function.name
        toolCallBufs[idx].argsBuf += tc.function?.arguments ?? ""
      }
      if (choice.finish_reason === "tool_calls") {
        const toolCalls = Object.values(toolCallBufs).map(tb => ({ id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}" }))
        this.rememberMiniMaxReplay(finalText, toolCalls, reasoningContent, reasoningDetails, nativeToolCallsFromBuffers(toolCallBufs))
        for (const [index, tb] of Object.entries(toolCallBufs)) {
          const idx = Number(index)
          if (emittedToolCallIndexes.has(idx)) continue
          let args: Record<string, unknown> = {}
          try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
          emittedToolCallIndexes.add(idx)
          yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
        }
      }
    }

    const toolCalls = Object.values(toolCallBufs).map(tb => ({ id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}" }))
    this.rememberMiniMaxReplay(finalText, toolCalls, reasoningContent, reasoningDetails, nativeToolCallsFromBuffers(toolCallBufs))
    for (const [index, tb] of Object.entries(toolCallBufs)) {
      const idx = Number(index)
      if (emittedToolCallIndexes.has(idx)) continue
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
      emittedToolCallIndexes.add(idx)
      yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
    }
    if (totalTokens > 0) yield { type: "usage", totalTokens, inputTokens, outputTokens } as StreamEvent
  }

  private rememberMiniMaxReplay(
    content: string,
    toolCalls: Array<{ id: string; name: string; arguments: string }>,
    reasoningContent: unknown,
    reasoningDetails: unknown,
    nativeToolCalls: unknown[],
  ): void {
    const hasReasoning = typeof reasoningContent === "string" && reasoningContent.trim().length > 0
    const hasDetails = reasoningDetails !== undefined && reasoningDetails !== null
    if (!hasReasoning && !hasDetails) return
    this.chat.rememberReplayFields({ content, toolCalls }, {
      schema_version: 2,
      provider: "minimax",
      protocol: "openai-chat",
      model: this.model,
      ...(hasReasoning ? { reasoning_content: reasoningContent } : {}),
      ...(hasDetails ? { reasoning_details: reasoningDetails } : {}),
      ...(nativeToolCalls.length ? { tool_calls: nativeToolCalls } : {}),
    })
  }
}

function nativeToolCallsFromBuffers(
  toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }>,
): Array<Record<string, unknown>> {
  return Object.values(toolCallBufs).map(tb => ({
    id: tb.id,
    type: "function",
    function: { name: tb.name, arguments: tb.argsBuf || "{}" },
  }))
}
