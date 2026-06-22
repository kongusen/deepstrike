import OpenAI from "openai"
import type { Message, ProviderDescriptor, ProviderReplay, ProviderRunState, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys, openAICachedPromptTokens, stablePromptCacheKey, ThinkingTagStreamExtractor } from "./base.js"
import { OpenAIChatAdapter } from "./openai-chat.js"
import type { ReplayabilityAssessment } from "./replay-validator.js"

const OPENAI_POLICIES: Record<string, RuntimePolicy> = {
  "gpt-5.5":       { maxTurns: 60 },
  "gpt-5.4":       { maxTurns: 50 },
  "gpt-5.4-mini":  { maxTurns: 25 },
  "gpt-5.4-nano":  { maxTurns: 15 },
  "gpt-5.2":       { maxTurns: 50 },
  "gpt-5.2-pro":   { maxTurns: 60 },
  "gpt-5.1":       { maxTurns: 50 },
  "gpt-4o":        { maxTurns: 25 },
  "gpt-4o-mini":   { maxTurns: 15 },
  "gpt-4.1":       { maxTurns: 35 },
  "gpt-4.1-mini":  { maxTurns: 20 },
  "gpt-4.1-nano":  { maxTurns: 15 },
  "gpt-5":         { maxTurns: 50 },
  "gpt-5-pro":     { maxTurns: 60 },
  "gpt-5-mini":    { maxTurns: 25 },
  "gpt-5-nano":    { maxTurns: 15 },
  "o1":            { maxTurns: 50 },
  "o1-mini":       { maxTurns: 25 },
  "o3":            { maxTurns: 50 },
  "o3-mini":       { maxTurns: 25 },
  "o4-mini":       { maxTurns: 25 },
}

/** Options-object form for `OpenAIProvider` — the recommended way to construct an OpenAI-compatible
 *  provider (custom `baseURL` no longer needs a positional hole). */
export interface OpenAIProviderOptions {
  apiKey: string
  model?: string
  retry?: { maxRetries: number; baseDelay: number }
  /** Custom OpenAI-compatible endpoint (MiMo, DeepSeek, Kimi, …). Defaults to the OpenAI API. */
  baseURL?: string
}

/** Reasoning captured from a single model turn, handed to the replay-remember hooks so an
 *  OpenAI-compatible subclass can persist whatever replay envelope its wire requires. */
export interface OpenAIChatTurnReasoning {
  reasoningContent: string
  reasoningDetails?: unknown
  nativeToolCalls: unknown[]
}

/** Rebuild OpenAI-native `tool_calls` blocks from the streamed buffers — needed by reasoning
 *  vendors (DeepSeek/MiniMax) that persist the native blocks in their replay envelope. */
export function nativeToolCallsFromBuffers(
  toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }>,
): Array<Record<string, unknown>> {
  return Object.values(toolCallBufs).map(tb => ({
    id: tb.id,
    type: "function",
    function: { name: tb.name, arguments: tb.argsBuf || "{}" },
  }))
}

export class OpenAIChatProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly model: string
  protected readonly chat = new OpenAIChatAdapter()

  // Accepts either the options object (`new OpenAIProvider({ apiKey, model, baseURL })`) or the legacy
  // positional form (still used by the backend subclasses' `super(...)` calls).
  constructor(
    apiKeyOrOptions: string | OpenAIProviderOptions,
    model = "gpt-4o",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
  ) {
    const o: Required<OpenAIProviderOptions> =
      typeof apiKeyOrOptions === "string"
        ? { apiKey: apiKeyOrOptions, model, retry, baseURL }
        : { model: "gpt-4o", retry: { maxRetries: 3, baseDelay: 1000 }, baseURL: "https://api.openai.com/v1", ...apiKeyOrOptions }
    this.model = o.model
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey: o.apiKey, baseURL: o.baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = o.retry.maxRetries
    this.baseDelay = o.retry.baseDelay
  }

  runtimePolicy(): RuntimePolicy {
    return OPENAI_POLICIES[this.model] ?? {}
  }

  descriptor(): ProviderDescriptor {
    return {
      provider: "openai",
      protocol: "openai-chat",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: false,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
    }
  }

  protected requireNonEmptyReasoningReplayForToolTurns(_extensions?: Record<string, unknown>): boolean {
    return false
  }

  protected degradeMissingReasoningReplay(extensions?: Record<string, unknown>): boolean {
    return extensions?.degradeMissingReasoningReplay === true
  }

  protected buildChatMessages(context: RenderedContext, extensions?: Record<string, unknown>) {
    return this.chat.buildMessages(context, {
      descriptor: this.descriptor(),
      requireNonEmptyReasoningForToolCalls: this.requireNonEmptyReasoningReplayForToolTurns(extensions),
      degradeMissingReasoning: this.degradeMissingReasoningReplay(extensions),
    })
  }

  // ── Template-Method hooks ───────────────────────────────────────────────────
  // Defaults reproduce the plain OpenAI-chat behavior; reasoning vendors
  // (DeepSeek/MiniMax) override these instead of duplicating complete()/stream().

  /** Pre-process caller extensions before they reach buildChatMessages + the wire request
   *  (e.g. set `__deepstrikeThinkingEnabled`). Default: pass through unchanged. */
  protected prepareExtensions(extensions?: Record<string, unknown>): Record<string, unknown> | undefined {
    return extensions
  }

  /** Extra top-level request-body fields merged into the chat.completions call (vendor thinking
   *  knobs like `reasoning_effort`, `extra_body`, `reasoning_split`). Default: none. */
  protected requestBodyExtras(_extensions?: Record<string, unknown>): Record<string, unknown> {
    return {}
  }

  /** Request-body params controlling prompt caching. Default sends OpenAI's `prompt_cache_key`;
   *  vendors whose endpoints reject unknown params (e.g. DeepSeek 400s) override to `{}`. */
  protected cacheKeyParams(context: RenderedContext, tools: ToolSchema[]): Record<string, unknown> {
    return { prompt_cache_key: this.promptCacheKey(context, tools) }
  }

  /** Whether streamed `content` may carry inline `<thinking>…</thinking>` tags to split out.
   *  Default true (OpenAI). Reasoning vendors emit reasoning out-of-band, so they return false. */
  protected usesInlineThinkingTags(): boolean {
    return true
  }

  /** Whether to surface streamed `reasoning_content` as thinking_delta events. Default true;
   *  vendors gate this behind an `exposeReasoning` extension. */
  protected exposeReasoningDelta(_extensions?: Record<string, unknown>): boolean {
    return true
  }

  /** Persist replay after a non-streaming turn. Default: nothing (plain OpenAI has no reasoning
   *  to replay). Reasoning vendors override to store their envelope. */
  protected rememberCompleteReplay(_content: string, _toolCalls: Array<{ id: string; name: string; arguments: string }>, _reasoning: OpenAIChatTurnReasoning): void {
    /* no-op */
  }

  /** Persist replay after a streamed turn. Default: store `{ reasoning_content }` when there is a
   *  tool-call turn or captured reasoning (the prior base behavior). Vendors override. */
  protected rememberStreamReplay(content: string, toolCalls: Array<{ id: string; name: string; arguments: string }>, reasoning: OpenAIChatTurnReasoning): void {
    if (toolCalls.length || reasoning.reasoningContent) {
      this.chat.rememberReplayFields({ content, toolCalls }, { reasoning_content: reasoning.reasoningContent })
    }
  }

  /**
   * Pre-flight query: would this history validate against this provider with the
   * given extensions, without sending the request? Lets an embedder route around
   * a reasoning-replay failure (keep thinking on, disable it, or skip this
   * candidate) before issuing the request. `ok: true` when this provider does
   * not require reasoning replay for the current extensions.
   */
  assessReplayability(context: RenderedContext, extensions?: Record<string, unknown>): ReplayabilityAssessment {
    if (!this.requireNonEmptyReasoningReplayForToolTurns(extensions)) {
      return { ok: true, offendingCallIds: [] }
    }
    return this.chat.assessReasoning(context)
  }

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const fields = this.chat.peekReplayFields(message)
    if (!fields || !("reasoning_content" in fields || "reasoning_details" in fields)) return undefined
    return fields as ProviderReplay
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.reasoning_content !== undefined || replay.reasoning_details !== undefined) {
      this.chat.rememberReplayFields(message, replay as Record<string, unknown>)
    }
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    const prepared = this.prepareExtensions(extensions)
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = this.buildChatMessages(context, prepared)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          ...this.cacheKeyParams(context, tools),
          ...this.requestExtensions(prepared),
          ...this.requestBodyExtras(extensions),
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message as OpenAI.ChatCompletionMessage & Record<string, unknown>
        const nativeToolCalls = choice.tool_calls ?? []
        const toolCalls = this.chat.normalizeToolCalls(nativeToolCalls as OpenAI.ChatCompletionMessageToolCall[])
        const content = choice.content ?? ""
        this.rememberCompleteReplay(content, toolCalls, {
          reasoningContent: typeof choice.reasoning_content === "string" ? choice.reasoning_content : "",
          reasoningDetails: choice.reasoning_details,
          nativeToolCalls: nativeToolCalls as unknown[],
        })
        return { role: "assistant", content, tokenCount: resp.usage?.completion_tokens ?? resp.usage?.total_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, _state?: ProviderRunState, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    const prepared = this.prepareExtensions(extensions)
    const msgs = this.buildChatMessages(context, prepared)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const emittedToolCallIndexes = new Set<number>()
    const useTags = this.usesInlineThinkingTags()
    const exposeReasoning = this.exposeReasoningDelta(extensions)
    const extractor = new ThinkingTagStreamExtractor()
    let accumulatedReasoning = ""
    let accumulatedReasoningDetails: unknown
    let accumulatedContent = ""

    const stream = await this.client.chat.completions.create({
      ...this.cacheKeyParams(context, tools),
      ...this.requestExtensions(prepared),
      ...this.requestBodyExtras(extensions),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
    // #2-B-ii: forward the abort signal so a preempt cancels the in-flight HTTP request.
    }, signal ? { signal } : undefined)

    const rememberStream = () => {
      const toolCalls = Object.values(toolCallBufs).map(tb => ({ id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}" }))
      this.rememberStreamReplay(accumulatedContent, toolCalls, {
        reasoningContent: accumulatedReasoning,
        reasoningDetails: accumulatedReasoningDetails,
        nativeToolCalls: nativeToolCallsFromBuffers(toolCallBufs),
      })
    }
    const emitPendingToolCalls = function* () {
      for (const [index, tb] of Object.entries(toolCallBufs)) {
        const idx = Number(index)
        if (emittedToolCallIndexes.has(idx)) continue
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
        emittedToolCallIndexes.add(idx)
        yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
      }
    }

    let totalTokens = 0
    let inputTokens = 0
    let outputTokens = 0
    let cacheReadTokens = 0
    for await (const chunk of stream) {
      if (chunk.usage) {
        totalTokens = chunk.usage.total_tokens
        inputTokens = chunk.usage.prompt_tokens ?? 0
        outputTokens = chunk.usage.completion_tokens ?? 0
        cacheReadTokens = openAICachedPromptTokens(chunk.usage)
        continue
      }
      const choice = chunk.choices[0]
      if (!choice) continue
      const delta = choice.delta as Record<string, unknown>
      if (!delta) continue

      if (delta.reasoning_content) {
        accumulatedReasoning += String(delta.reasoning_content)
        if (exposeReasoning) yield { type: "thinking_delta", delta: String(delta.reasoning_content) } as ThinkingDelta
      }
      if (delta.reasoning_details !== undefined && delta.reasoning_details !== null) accumulatedReasoningDetails = delta.reasoning_details

      if (delta.content) {
        if (useTags) {
          for (const part of extractor.feed(String(delta.content))) {
            if (part.type === "thinking") {
              accumulatedReasoning += part.content
              yield { type: "thinking_delta", delta: part.content } as ThinkingDelta
            } else {
              accumulatedContent += part.content
              yield { type: "text_delta", delta: part.content } as TextDelta
            }
          }
        } else {
          accumulatedContent += String(delta.content)
          yield { type: "text_delta", delta: delta.content } as TextDelta
        }
      }

      for (const tc of (delta.tool_calls as OpenAI.ChatCompletionChunk.Choice.Delta.ToolCall[] | undefined) ?? []) {
        const idx = tc.index
        if (!toolCallBufs[idx]) toolCallBufs[idx] = { id: tc.id ?? "", name: "", argsBuf: "" }
        if (tc.function?.name) toolCallBufs[idx].name += tc.function.name
        toolCallBufs[idx].argsBuf += tc.function?.arguments ?? ""
      }

      if (choice.finish_reason === "tool_calls") {
        rememberStream()
        yield* emitPendingToolCalls()
      }
    }

    if (useTags) {
      for (const part of extractor.flush()) {
        if (part.type === "thinking") {
          accumulatedReasoning += part.content
          yield { type: "thinking_delta", delta: part.content } as ThinkingDelta
        } else {
          accumulatedContent += part.content
          yield { type: "text_delta", delta: part.content } as TextDelta
        }
      }
    }

    rememberStream()
    yield* emitPendingToolCalls()
    if (totalTokens > 0) yield { type: "usage", totalTokens, inputTokens, outputTokens, ...(cacheReadTokens > 0 ? { cacheReadInputTokens: cacheReadTokens } : {}) } as StreamEvent
  }

  /**
   * Default `prompt_cache_key` derived from the cacheable prefix (system prompt +
   * tool names) so requests for the same agent config route to the same cache.
   * A caller-supplied `prompt_cache_key` in extensions overrides it (it is spread
   * after this default). Unknown to non-OpenAI compatible endpoints, which ignore it.
   */
  protected promptCacheKey(context: RenderedContext, tools: ToolSchema[]): string {
    return stablePromptCacheKey([context.systemText, tools.map(t => t.name).join(",")])
  }

  protected requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, ["model", "messages", "tools", "stream", "stream_options", "__deepstrikeThinkingEnabled"])
  }
}

export { OpenAIChatProvider as OpenAIProvider }
