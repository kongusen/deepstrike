import Anthropic from "@anthropic-ai/sdk"
import type { Message, ProviderDescriptor, ProviderReplay, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, normalizeToolCall, omitExtensionKeys, toAnthropicContent, toAnthropicMessages } from "./base.js"

const CLAUDE_POLICIES: Record<string, RuntimePolicy> = {
  "claude-opus-4-1":          { maxTurns: 50 },
  "claude-opus-4-7":          { maxTurns: 50 },
  "claude-opus-4-6":          { maxTurns: 50 },
  "claude-opus-4-0":          { maxTurns: 50 },
  "claude-sonnet-4-6":        { maxTurns: 25 },
  "claude-sonnet-4-0":        { maxTurns: 25 },
  "claude-haiku-4-5":         { maxTurns: 15 },
  "claude-3-5-haiku-latest":  { maxTurns: 15 },
}

interface AnthropicProviderOptions {
  baseURL?: string
  authMode?: "api-key" | "bearer"
}

export class AnthropicProvider implements LLMProvider {
  private client: Anthropic
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number
  private nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()

  constructor(
    apiKey: string,
    protected readonly model = "claude-sonnet-4-6",
    retry = { maxRetries: 3, baseDelay: 1000 },
    options: AnthropicProviderOptions = {},
  ) {
    this.client = withServerRuntimeGuard(() => new Anthropic({
      ...(options.authMode === "bearer"
        ? { authToken: apiKey, apiKey: null as unknown as string }
        : { apiKey, authToken: null as unknown as string }),
      ...(options.baseURL ? { baseURL: options.baseURL } : {}),
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  runtimePolicy(): RuntimePolicy {
    return CLAUDE_POLICIES[this.model] ?? {}
  }

  /** Identity advertised in the descriptor; overridden by Anthropic-compatible vendors (e.g. MiniMax). */
  protected providerName(): string {
    return "anthropic"
  }

  descriptor(): ProviderDescriptor {
    return {
      provider: this.providerName(),
      protocol: "anthropic-messages",
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

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.native_blocks?.length) {
      this.nativeAssistantBlocks.set(assistantReplayKey(message), replay.native_blocks)
      return
    }
    // Legacy log without persisted native blocks: reconstruct neutral
    // text + tool_use blocks from the transcript so a tool-use turn can be
    // replayed. Thinking blocks were never persisted, so they are not recovered.
    const blocks = reconstructAnthropicBlocks(message)
    if (blocks.length) this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }

  /**
   * Build tool definitions. A cache breakpoint is anchored on the final tool
   * only when the system blocks won't carry one (`anchorCache`). When structured
   * system blocks are present, their breakpoints already cache the tools prefix
   * (tools render before system), so a redundant tool breakpoint would only burn
   * one of Anthropic's 4 cache_control slots — slots the message history needs.
   */
  private buildTools(tools: ToolSchema[], anchorCache: boolean) {
    return tools.map((t, i) => ({
      name: t.name,
      description: t.description,
      input_schema: JSON.parse(t.parameters),
      ...(anchorCache && i === tools.length - 1 ? { cache_control: { type: "ephemeral" as const } } : {}),
    }))
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const system = this.buildSystem(context)
    const msgs = this.buildMessages(context)
    assertCacheBudget(system, tools.length)
    const requestExtensions = this.requestExtensions(extensions)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.createMessage({
          ...requestExtensions,
          model: this.model,
          max_tokens: typeof extensions?.max_tokens === "number" ? extensions.max_tokens : 8096,
          ...(system ? { system } : {}),
          messages: msgs,
          ...(tools.length ? { tools: this.buildTools(tools, !Array.isArray(system)) } : {}),
        }, extensions)
        this.circuit.recordSuccess()
        let content = ""
        const toolCalls = []
        for (const block of resp.content) {
          if (block.type === "text") content += block.text
          else if (block.type === "tool_use") {
            const tc = normalizeToolCall(block.id, block.name, block.input)
            if (tc) toolCalls.push(tc)
          }
        }
        const message = { role: "assistant" as const, content, tokenCount: resp.usage.output_tokens, toolCalls }
        this.rememberNativeBlocks(message, resp.content as unknown as Array<Record<string, unknown>>)
        return message
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = this.buildSystem(context)
    const msgs = this.buildMessages(context)
    assertCacheBudget(system, tools.length)
    const requestExtensions = this.requestExtensions(extensions)
    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const nativeBlocks: Record<number, Record<string, unknown>> = {}
    let finalText = ""
    const finalToolCalls: Array<{ id: string; name: string; arguments: string }> = []

    const stream = this.streamMessage({
      ...requestExtensions,
      model: this.model,
      max_tokens: typeof extensions?.max_tokens === "number" ? extensions.max_tokens : 8096,
      ...(system ? { system } : {}),
      messages: msgs,
      ...(tools.length ? { tools: this.buildTools(tools, !Array.isArray(system)) } : {}),
    }, extensions)

    let uncachedInput = 0
    let cacheReadTokens = 0
    let cacheCreationTokens = 0
    let outputTokens = 0
    for await (const evt of stream) {
      if (evt.type === "message_start" || evt.type === "message_delta") {
        const usage = evt.usage ?? evt.message?.usage
        if (usage) {
          // input + cache counts are cumulative and pinned at message_start; a
          // later message_delta may omit them (null), so Math.max keeps the
          // running totals from being clobbered back to zero.
          uncachedInput = Math.max(uncachedInput, usage.input_tokens ?? 0)
          cacheReadTokens = Math.max(cacheReadTokens, usage.cache_read_input_tokens ?? 0)
          cacheCreationTokens = Math.max(cacheCreationTokens, usage.cache_creation_input_tokens ?? 0)
          outputTokens = Math.max(outputTokens, usage.output_tokens ?? 0)
          // inputTokens is the FULL prompt size (uncached + cache read + cache
          // write). The kernel reads it as the authoritative prompt size for
          // context-pressure/compaction — excluding cached tokens would make a
          // cache-heavy turn look tiny and suppress compaction until a 413.
          const inputTokens = uncachedInput + cacheReadTokens + cacheCreationTokens
          yield {
            type: "usage",
            totalTokens: inputTokens + outputTokens,
            inputTokens,
            outputTokens,
            cacheReadInputTokens: cacheReadTokens,
            cacheCreationInputTokens: cacheCreationTokens,
          } as UsageEvent
        }
      } else if (evt.type === "content_block_start") {
        nativeBlocks[evt.index] = { ...(evt.content_block as unknown as Record<string, unknown>) }
        if (evt.content_block.type === "tool_use") {
          toolBlocks[evt.index] = { id: evt.content_block.id, name: evt.content_block.name, argsBuf: "" }
        }
      } else if (evt.type === "content_block_delta") {
        const d = evt.delta
        if (d.type === "text_delta") {
          finalText += d.text
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], text: String(nativeBlocks[evt.index]?.text ?? "") + d.text }
          yield { type: "text_delta", delta: d.text } as TextDelta
        } else if (d.type === "thinking_delta") {
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], thinking: String(nativeBlocks[evt.index]?.thinking ?? "") + d.thinking }
          yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
        } else if (d.type === "signature_delta") {
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], signature: String(nativeBlocks[evt.index]?.signature ?? "") + d.signature }
        } else if (d.type === "input_json_delta" && toolBlocks[evt.index]) {
          toolBlocks[evt.index].argsBuf += d.partial_json
        }
      } else if (evt.type === "content_block_stop" && toolBlocks[evt.index] !== undefined) {
        const tb = toolBlocks[evt.index]
        delete toolBlocks[evt.index]
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
        nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], input: args }
        finalToolCalls.push({ id: tb.id, name: tb.name, arguments: JSON.stringify(args) })
        yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
      }
    }

    this.rememberNativeBlocks(
      { content: finalText, toolCalls: finalToolCalls },
      Object.keys(nativeBlocks).map(Number).sort((a, b) => a - b).map(index => nativeBlocks[index]),
    )
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, ["model", "messages", "system", "tools", "max_tokens", "stream"])
  }

  private hasBetas(extensions?: Record<string, unknown>): boolean {
    const betas = extensions?.betas
    return Array.isArray(betas) && betas.length > 0
  }

  private createMessage(
    params: Record<string, unknown>,
    extensions?: Record<string, unknown>,
  ): Promise<any> {
    return this.hasBetas(extensions)
      ? this.client.beta.messages.create(params as unknown as Parameters<typeof this.client.beta.messages.create>[0])
      : this.client.messages.create(params as unknown as Anthropic.MessageCreateParamsNonStreaming)
  }

  private streamMessage(
    params: Record<string, unknown>,
    extensions?: Record<string, unknown>,
  ): AsyncIterable<any> {
    return (this.hasBetas(extensions)
      ? this.client.beta.messages.stream(params as unknown as Parameters<typeof this.client.beta.messages.stream>[0])
      : this.client.messages.stream(params as unknown as Anthropic.MessageStreamParams)
    ) as unknown as AsyncIterable<any>
  }

  private buildSystem(context: RenderedContext): Array<{ type: "text"; text: string; cache_control?: { type: "ephemeral" } }> | string | undefined {
    // B3 note: the system shape is content-driven — 0 blocks (string), 1 block
    // (stable only), or 2 blocks (stable + knowledge). The first turn `systemKnowledge`
    // appears, the block count rises 1→2, which is a one-time prompt-cache invalidation
    // (the knowledge prefix didn't exist to cache before). It is byte-stable thereafter;
    // dynamic per-turn knowledge belongs in the uncached tail, not this block. An empty
    // knowledge string is intentionally never emitted (the API rejects empty text blocks).
    if (!context.systemStable && !context.systemKnowledge) {
      return context.systemText || undefined
    }
    const blocks: Array<{ type: "text"; text: string; cache_control: { type: "ephemeral" } }> = []
    if (context.systemStable) {
      blocks.push({ type: "text", text: context.systemStable, cache_control: { type: "ephemeral" } })
    }
    if (context.systemKnowledge) {
      blocks.push({ type: "text", text: context.systemKnowledge, cache_control: { type: "ephemeral" } })
    }
    return blocks.length ? blocks : undefined
  }

  private buildMessages(context: RenderedContext): Anthropic.MessageParam[] {
    const msgs = toAnthropicMessages(context.turns, message =>
      this.nativeAssistantBlocks.get(assistantReplayKey(message))
    ) as unknown as Anthropic.MessageParam[]

    // Cache breakpoints anchor on the stable history; the volatile State turn is
    // appended AFTER them as the uncached tail (so the history prefix re-reads
    // across turns). On un-rebuilt bindings stateTurn is absent and the state is
    // already inside `turns` — rendered as-is above. `frozenPrefixLen` (P1-E) pins
    // the deep breakpoint at the compaction boundary; absent ⇒ rolling-pair fallback.
    applyMessageCacheControl(msgs, context.frozenPrefixLen)
    if (context.stateTurn) {
      // Render through toAnthropicMessages so assistant tool_use blocks and
      // tool-role tool_result parts are serialized correctly — toAnthropicContent
      // only handles contentParts/content and would silently drop toolCalls.
      const stateMsgs = toAnthropicMessages([context.stateTurn], message =>
        this.nativeAssistantBlocks.get(assistantReplayKey(message))
      ) as unknown as Anthropic.MessageParam[]
      msgs.push(...stateMsgs)
    }

    if (msgs.length === 0) {
      msgs.push({ role: "user", content: "Proceed." })
    }

    return msgs
  }

  private rememberNativeBlocks(
    message: Pick<Message, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!blocks.length) return
    if (!message.toolCalls?.length && !blocks.some(b => b.type === "thinking")) return
    this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }
}

/** Anthropic accepts at most this many cache_control breakpoints per request. */
const MAX_CACHE_BREAKPOINTS = 4

/**
 * Number of rolling cache breakpoints to spend on the message history. Anthropic
 * allows 4 cache_control breakpoints total; the static system/tools prefix
 * consumes up to 2 (systemStable + systemKnowledge), leaving 2 for the history.
 */
const MESSAGE_CACHE_BREAKPOINTS = 2

/**
 * Regression guard: fail loudly if the static (system + tools) breakpoints plus
 * the rolling message budget could exceed Anthropic's hard limit, instead of
 * letting the API reject the request with an opaque 400. Uses the worst-case
 * message count (`MESSAGE_CACHE_BREAKPOINTS`), so it can only fire if a future
 * change adds a system partition or raises the message budget.
 */
function assertCacheBudget(system: unknown, toolCount: number): void {
  const systemBreakpoints = Array.isArray(system) ? system.length : 0
  const toolBreakpoints = toolCount > 0 && !Array.isArray(system) ? 1 : 0
  const worstCase = systemBreakpoints + toolBreakpoints + MESSAGE_CACHE_BREAKPOINTS
  if (worstCase > MAX_CACHE_BREAKPOINTS) {
    throw new Error(
      `Anthropic cache_control budget exceeded: ${systemBreakpoints} system + ${toolBreakpoints} tool + ${MESSAGE_CACHE_BREAKPOINTS} message > ${MAX_CACHE_BREAKPOINTS}`,
    )
  }
}

/**
 * Place the (≤2) message-history cache breakpoints. The final message always gets
 * one — it writes the current full prefix for the next turn to read. The second is
 * placed by one of two strategies:
 *
 *   • **Deep anchor (P1-E)** — when `frozenPrefixLen` marks a distinct frozen prefix
 *     (the compaction boundary), pin the second breakpoint there. It is byte-stable
 *     across turns, so `[0..frozen]` is re-read cheaply every turn and is immune to
 *     the 20-block lookback miss that strikes heavy tool turns (>20 blocks/turn); the
 *     tail breakpoint then writes only the incremental `[frozen..tail]`.
 *   • **Rolling fallback** — otherwise (older binding / no compaction yet / whole
 *     render hot), roll the second breakpoint to the nearest preceding user turn, the
 *     previous turn's read anchor (Anthropic's 20-block lookback bridges light turns).
 *
 * Without any of this the cached prefix stops at the end of `system` and every turn
 * re-bills the entire tool-result history at full price (~quadratic cumulative cost).
 * cache_control attaches to the last content block of each target, promoting a bare
 * string body to a text block.
 */
function applyMessageCacheControl(msgs: Anthropic.MessageParam[], frozenPrefixLen?: number): void {
  if (!msgs.length) return
  const targets = new Set<number>([msgs.length - 1])
  if (typeof frozenPrefixLen === "number" && frozenPrefixLen >= 1 && frozenPrefixLen < msgs.length) {
    // Deep anchor at the frozen-prefix boundary (last frozen turn). Fixed between compactions.
    targets.add(frozenPrefixLen - 1)
  } else {
    for (let i = msgs.length - 2; i >= 0 && targets.size < MESSAGE_CACHE_BREAKPOINTS; i--) {
      if (msgs[i].role === "user") targets.add(i)
    }
  }
  for (const idx of targets) markLastBlockCacheable(msgs[idx])
}

/** Attach an ephemeral cache breakpoint to a message's final content block. */
function markLastBlockCacheable(msg: Anthropic.MessageParam): void {
  const cache_control = { type: "ephemeral" as const }
  if (typeof msg.content === "string") {
    if (!msg.content) return // don't synthesize an empty (API-rejected) text block
    msg.content = [{ type: "text", text: msg.content, cache_control }]
    return
  }
  if (Array.isArray(msg.content) && msg.content.length) {
    const last = msg.content[msg.content.length - 1] as { cache_control?: { type: "ephemeral" } }
    last.cache_control = cache_control
  }
}

/**
 * Reconstruct Anthropic assistant content blocks from a neutral transcript when
 * no provider replay was persisted. Only meaningful for tool-use turns: a plain
 * text turn needs no native blocks to replay.
 */
function reconstructAnthropicBlocks(
  message: Pick<Message, "content" | "toolCalls">,
): Array<Record<string, unknown>> {
  const toolCalls = message.toolCalls ?? []
  if (!toolCalls.length) return []
  const blocks: Array<Record<string, unknown>> = []
  if (message.content) blocks.push({ type: "text", text: message.content })
  for (const tc of toolCalls) {
    let input: Record<string, unknown> = {}
    try { input = JSON.parse(tc.arguments || "{}") as Record<string, unknown> } catch { input = {} }
    blocks.push({ type: "tool_use", id: tc.id, name: tc.name, input })
  }
  return blocks
}
