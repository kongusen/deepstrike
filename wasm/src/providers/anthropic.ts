import type { RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent, LLMProvider, Message, ProviderDescriptor, ProviderReplay } from "../types.js"
import { assistantReplayKey, collectStreamMessage, toAnthropicMessages } from "./base.js"

/** Anthropic accepts at most this many cache_control breakpoints per request. */
const MAX_CACHE_BREAKPOINTS = 4
/** Rolling cache breakpoints reserved for the message history (system uses ≤2). */
const MESSAGE_CACHE_BREAKPOINTS = 2

function buildAnthropicTools(tools: ToolSchema[], anchorCache: boolean) {
  return tools.map((t, i) => ({
    name: t.name,
    description: t.description,
    input_schema: JSON.parse(t.parameters),
    // Anchor a tool breakpoint only when the system blocks won't carry one;
    // otherwise systemStable already caches the tools prefix (tools render
    // first), and a redundant tool breakpoint would burn a slot the message
    // history needs to stay within the 4-breakpoint budget.
    ...(anchorCache && i === tools.length - 1 ? { cache_control: { type: "ephemeral" as const } } : {}),
  }))
}

/**
 * Roll cache breakpoints across the conversation tail so the message-history
 * prefix is written once and re-read on later turns (without this the cached
 * prefix stops at the end of `system` and the whole tool-result history is
 * re-billed at full input price every turn).
 *
 * When `frozenPrefixLen` marks a distinct frozen prefix (the compaction
 * boundary), pin the deep breakpoint there — it is byte-stable across turns,
 * so `[0..frozen]` is re-read cheaply every turn; the tail breakpoint writes
 * only the incremental `[frozen..tail]`. Otherwise fall back to the rolling
 * pair: final message + nearest preceding user turn.
 */
function applyMessageCacheControl(msgs: Array<Record<string, unknown>>, frozenPrefixLen?: number): void {
  if (!msgs.length) return
  const targets = new Set<number>([msgs.length - 1])
  if (typeof frozenPrefixLen === "number" && frozenPrefixLen >= 1 && frozenPrefixLen < msgs.length) {
    targets.add(frozenPrefixLen - 1)
  } else {
    for (let i = msgs.length - 2; i >= 0 && targets.size < MESSAGE_CACHE_BREAKPOINTS; i--) {
      if (msgs[i].role === "user") targets.add(i)
    }
  }
  for (const idx of targets) markLastBlockCacheable(msgs[idx])
}

function markLastBlockCacheable(msg: Record<string, unknown>): void {
  const cache_control = { type: "ephemeral" as const }
  if (typeof msg.content === "string") {
    if (!msg.content) return
    msg.content = [{ type: "text", text: msg.content, cache_control }]
    return
  }
  if (Array.isArray(msg.content) && msg.content.length) {
    const last = msg.content[msg.content.length - 1] as Record<string, unknown>
    last.cache_control = cache_control
  }
}

/** Regression guard: fail loudly before the API would reject the request for
 *  exceeding the cache_control breakpoint limit. */
function assertCacheBudget(system: unknown, toolCount: number): void {
  const systemBreakpoints = Array.isArray(system) ? system.length : 0
  const toolBreakpoints = toolCount > 0 && !Array.isArray(system) ? 1 : 0
  if (systemBreakpoints + toolBreakpoints + MESSAGE_CACHE_BREAKPOINTS > MAX_CACHE_BREAKPOINTS) {
    throw new Error(
      `Anthropic cache_control budget exceeded: ${systemBreakpoints} system + ${toolBreakpoints} tool + ${MESSAGE_CACHE_BREAKPOINTS} message > ${MAX_CACHE_BREAKPOINTS}`,
    )
  }
}

export class AnthropicProvider implements LLMProvider {
  private nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()

  constructor(
    private readonly apiKey: string,
    private readonly model = "claude-sonnet-4-6",
    private readonly maxTokens = 8096,
  ) {}

  descriptor(): ProviderDescriptor {
    return {
      provider: "anthropic",
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
    // text + tool_use blocks so a tool-use turn can be replayed.
    const blocks = reconstructAnthropicBlocks(message)
    if (blocks.length) this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    return collectStreamMessage(this.stream(context, tools, extensions))
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const systemBlocks: Array<{ type: "text"; text: string; cache_control?: { type: "ephemeral" } }> = []
    if (context.systemStable) {
      systemBlocks.push({ type: "text", text: context.systemStable, cache_control: { type: "ephemeral" } })
    }
    if (context.systemKnowledge) {
      systemBlocks.push({ type: "text", text: context.systemKnowledge, cache_control: { type: "ephemeral" } })
    }
    const system = systemBlocks.length ? systemBlocks : (context.systemText || undefined)
    const msgs = toAnthropicMessages(context, message =>
      this.nativeAssistantBlocks.get(assistantReplayKey(message))
    )
    applyMessageCacheControl(msgs, context.frozenPrefixLen)
    // Append the volatile State turn AFTER the cache breakpoints (uncached tail);
    // absent on un-rebuilt bindings, where the state is already inside `turns`.
    // Render through toAnthropicMessages so assistant tool_use blocks are
    // serialized correctly — raw content would silently drop toolCalls.
    if (context.stateTurn) {
      const stateCtx: RenderedContext = { systemText: "", turns: [context.stateTurn] }
      const stateMsgs = toAnthropicMessages(stateCtx, message =>
        this.nativeAssistantBlocks.get(assistantReplayKey(message))
      )
      msgs.push(...stateMsgs)
    }
    assertCacheBudget(system, tools.length)

    const body: Record<string, unknown> = {
      model: this.model,
      max_tokens: this.maxTokens,
      messages: msgs,
      stream: true,
      ...(system ? { system } : {}),
      ...(tools.length ? { tools: buildAnthropicTools(tools, !Array.isArray(system)) } : {}),
    }
    if (extensions?.enable_thinking) {
      body.thinking = { type: "enabled", budget_tokens: 8000 }
    }

    const resp = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: {
        "x-api-key": this.apiKey,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
        "anthropic-beta": "prompt-caching-2024-07-31",
      },
      body: JSON.stringify(body),
    })
    if (!resp.ok) throw new Error(`Anthropic ${resp.status}: ${await resp.text()}`)

    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const nativeBlocks: Record<number, Record<string, unknown>> = {}
    let finalText = ""
    const finalToolCalls: Array<{ id: string; name: string; arguments: string }> = []
    const reader = resp.body!.getReader()
    const decoder = new TextDecoder()
    let buf = ""
    let uncachedInput = 0
    let cacheReadTokens = 0
    let cacheCreationTokens = 0
    let outputTokens = 0

    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split("\n")
      buf = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.startsWith("data: ")) continue
        const data = line.slice(6).trim()
        if (data === "[DONE]") return
        try {
          const evt = JSON.parse(data) as Record<string, unknown>
          if (evt.type === "message_start" || evt.type === "message_delta") {
            const usage = (evt.usage ?? (evt.message as Record<string, unknown> | undefined)?.usage) as
              | { input_tokens?: number; output_tokens?: number; cache_read_input_tokens?: number; cache_creation_input_tokens?: number }
              | undefined
            if (usage) {
              // input + cache counts are pinned at message_start; a later
              // message_delta may omit them — Math.max prevents zeroing.
              uncachedInput = Math.max(uncachedInput, usage.input_tokens ?? 0)
              cacheReadTokens = Math.max(cacheReadTokens, usage.cache_read_input_tokens ?? 0)
              cacheCreationTokens = Math.max(cacheCreationTokens, usage.cache_creation_input_tokens ?? 0)
              outputTokens = Math.max(outputTokens, usage.output_tokens ?? 0)
              // inputTokens is the FULL prompt (uncached + cache read + write):
              // the kernel reads it as the authoritative context size.
              const inputTokens = uncachedInput + cacheReadTokens + cacheCreationTokens
              if (inputTokens > 0 || outputTokens > 0) {
                yield {
                  type: "usage",
                  totalTokens: inputTokens + outputTokens,
                  inputTokens,
                  outputTokens,
                  cacheReadInputTokens: cacheReadTokens,
                  cacheCreationInputTokens: cacheCreationTokens,
                } as UsageEvent
              }
            }
          } else if (evt.type === "content_block_start") {
            const idx = evt.index as number
            nativeBlocks[idx] = { ...(evt.content_block as Record<string, unknown>) }
            const cb = evt.content_block as Record<string, unknown>
            if (cb.type === "tool_use")
              toolBlocks[idx] = { id: cb.id as string, name: cb.name as string, argsBuf: "" }
          } else if (evt.type === "content_block_delta") {
            const d = evt.delta as Record<string, unknown>
            const idx = evt.index as number
            if (d.type === "text_delta") {
              finalText += String(d.text)
              nativeBlocks[idx] = { ...nativeBlocks[idx], text: String(nativeBlocks[idx]?.text ?? "") + d.text }
              yield { type: "text_delta", delta: d.text } as TextDelta
            } else if (d.type === "thinking_delta") {
              nativeBlocks[idx] = { ...nativeBlocks[idx], thinking: String(nativeBlocks[idx]?.thinking ?? "") + d.thinking }
              yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
            } else if (d.type === "signature_delta") {
              nativeBlocks[idx] = { ...nativeBlocks[idx], signature: String(nativeBlocks[idx]?.signature ?? "") + d.signature }
            } else if (d.type === "input_json_delta" && toolBlocks[idx]) {
              toolBlocks[idx].argsBuf += d.partial_json
            }
          } else if (evt.type === "content_block_stop") {
            const idx = evt.index as number
            if (toolBlocks[idx]) {
              const tb = toolBlocks[idx]
              delete toolBlocks[idx]
              let args: Record<string, unknown> = {}
              try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
              nativeBlocks[idx] = { ...nativeBlocks[idx], input: args }
              finalToolCalls.push({ id: tb.id, name: tb.name, arguments: JSON.stringify(args) })
              yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
            }
          }
        } catch { /* skip malformed */ }
      }
    }

    this.rememberNativeBlocks({ content: finalText, toolCalls: finalToolCalls }, Object.keys(nativeBlocks).map(Number).sort((a, b) => a - b).map(index => nativeBlocks[index]))
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

/**
 * Reconstruct Anthropic assistant content blocks from a neutral transcript when
 * no provider replay was persisted. Only meaningful for tool-use turns.
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
