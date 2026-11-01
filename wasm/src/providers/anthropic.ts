import { requestSnapshot } from "./prepared-request.js"
import type { PreparedProviderRequest, ProviderRunState } from "../types.js"
import type { CacheBreakpointStrategy, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent, LLMProvider, ModelMessage, ProviderDescriptor, ProviderReplay } from "../types.js"
import { assistantReplayKey, collectStreamMessage, toAnthropicMessages } from "./base.js"
import { malformedToolArguments, toolArgumentsText } from "../runtime/tool-arguments.js"
import {
  OnDemandCompactor,
  THRESHOLD_COMPACTION_BETA,
  compactionBlock,
  resolveCompaction,
  resumeAfterCompactionPause,
  withoutCompactionPause,
  type AnthropicCompaction,
} from "./anthropic-compaction.js"

export type {
  AnthropicCompaction,
  AnthropicOnDemandCompaction,
  AnthropicThresholdCompaction,
} from "./anthropic-compaction.js"

/** Anthropic accepts at most this many cache_control breakpoints per request. */
const MAX_CACHE_BREAKPOINTS = 4
/** Rolling cache breakpoints reserved for the message history (system uses ≤2). */
const MESSAGE_CACHE_BREAKPOINTS = 2

function buildAnthropicTools(tools: ToolSchema[], anchorCache: boolean, strategy: CacheBreakpointStrategy) {
  const emitOnLastTool = anchorCache && (strategy === "default" || strategy === "tools-only")
  return tools.map((t, i) => ({
    name: t.name,
    description: t.description,
    input_schema: JSON.parse(t.parameters),
    ...(emitOnLastTool && i === tools.length - 1 ? { cache_control: { type: "ephemeral" as const } } : {}),
  }))
}

/** Recognised values for the `cacheBreakpointStrategy` extension; unknown → "default". */
const CACHE_BREAKPOINT_STRATEGIES = new Set<CacheBreakpointStrategy>([
  "default", "tools-only", "system-only", "frozen-prefix", "none",
])
function resolveCacheBreakpointStrategy(extensions?: Record<string, unknown>): CacheBreakpointStrategy {
  const raw = extensions?.cacheBreakpointStrategy
  if (typeof raw === "string" && CACHE_BREAKPOINT_STRATEGIES.has(raw as CacheBreakpointStrategy)) {
    return raw as CacheBreakpointStrategy
  }
  return "default"
}

/** I1: which slots of the outgoing request carry a `cache_control` breakpoint — used to pro-rata
 *  attribute the response's `cache_read_input_tokens` (a single scalar). Mirrors Node. */
function countCacheControlSlots(
  system: undefined | string | Array<{ cache_control?: unknown }>,
  builtTools: undefined | Array<{ cache_control?: unknown }>,
  msgs: Array<Record<string, unknown>>,
): { system: boolean; tools: boolean; messages: boolean } {
  const sysBp = Array.isArray(system) && system.some(b => b?.cache_control != null)
  const toolBp = !!builtTools && builtTools.some(t => t?.cache_control != null)
  let msgBp = false
  for (const m of msgs) {
    if (Array.isArray(m.content)) {
      if ((m.content as Array<{ cache_control?: unknown }>).some(b => b?.cache_control != null)) {
        msgBp = true
        break
      }
    }
  }
  return { system: sysBp, tools: toolBp, messages: msgBp }
}

/** I1: split `cache_read_input_tokens` evenly across contributing slots. Remainder lands on the
 *  first contributing slot to keep the sum exact. Mirrors Node. */
function estimateCacheReadBySlot(
  cacheRead: number,
  slotBp: { system: boolean; tools: boolean; messages: boolean },
): { system?: number; tools?: number; messages?: number } | undefined {
  if (cacheRead <= 0) return undefined
  const count = (slotBp.system ? 1 : 0) + (slotBp.tools ? 1 : 0) + (slotBp.messages ? 1 : 0)
  if (count === 0) return undefined
  const share = Math.floor(cacheRead / count)
  const remainder = cacheRead - share * count
  const out: { system?: number; tools?: number; messages?: number } = {}
  let firstDone = false
  const give = (): number => { if (!firstDone) { firstDone = true; return share + remainder } return share }
  if (slotBp.system) out.system = give()
  if (slotBp.tools) out.tools = give()
  if (slotBp.messages) out.messages = give()
  return out
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
function applyMessageCacheControl(msgs: Array<Record<string, unknown>>, frozenPrefixLen: number | undefined, strategy: CacheBreakpointStrategy): void {
  if (!msgs.length) return
  if (strategy === "tools-only" || strategy === "system-only" || strategy === "none") return
  const targets = new Set<number>([msgs.length - 1])
  if (typeof frozenPrefixLen === "number" && frozenPrefixLen >= 1 && frozenPrefixLen < msgs.length) {
    targets.add(frozenPrefixLen - 1)
  } else if (strategy === "default") {
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

/** `clear_tool_uses_20250919` settings. Unset fields take the API defaults. */
export interface AnthropicContextEditing {
  /** Input tokens above which clearing starts. */
  triggerInputTokens?: number
  /** Most recent tool uses kept intact. */
  keepToolUses?: number
  /** Skip a clearing that would remove fewer tokens than this — each clearing rewrites the cached prefix. */
  clearAtLeastInputTokens?: number
  /** Tools whose uses and results are never cleared. */
  excludeTools?: string[]
  /** Also clear the tool-call inputs, not only the results. */
  clearToolInputs?: boolean
}

export interface AnthropicProviderOptions {
  /** Clear old tool results server-side (context editing). The provider then declares the tier
   *  native and the kernel stops clearing tool results itself; `true` takes the API defaults. */
  contextEditing?: true | AnthropicContextEditing
  /** Summarize earlier history server-side: at a threshold on every request (`compact_20260112`,
   *  the default mode), or on demand once a turn passes a limit (`compact-2026-09-04`, `mode:
   *  "on_demand"`). The provider then declares compaction native and the kernel stops compacting
   *  itself; its renewal stays the safety net, so set the trigger below the kernel's renewal
   *  threshold. */
  compaction?: true | AnthropicCompaction
}

const CONTEXT_MANAGEMENT_BETA = "context-management-2025-06-27"
const PROMPT_CACHING_BETA = "prompt-caching-2024-07-31"

/** What one streamed response left behind once its events were yielded. */
interface StreamOutcome {
  nativeBlocks: Array<Record<string, unknown>>
  finalText: string
  finalToolCalls: Array<{ id: string; name: string; arguments: string }>
  stopReason?: string
  totalTokens: number
}

/** The leading wire messages that are history; the state turn follows them. */
function historyCount(wireTurns: number[]): number {
  return wireTurns.filter(index => index >= 0).length
}

function clearToolUsesEdit(config: true | AnthropicContextEditing): Record<string, unknown> {
  const c = config === true ? {} : config
  return {
    type: "clear_tool_uses_20250919",
    ...(c.triggerInputTokens !== undefined ? { trigger: { type: "input_tokens", value: c.triggerInputTokens } } : {}),
    ...(c.keepToolUses !== undefined ? { keep: { type: "tool_uses", value: c.keepToolUses } } : {}),
    ...(c.clearAtLeastInputTokens !== undefined ? { clear_at_least: { type: "input_tokens", value: c.clearAtLeastInputTokens } } : {}),
    ...(c.excludeTools?.length ? { exclude_tools: [...c.excludeTools] } : {}),
    ...(c.clearToolInputs !== undefined ? { clear_tool_inputs: c.clearToolInputs } : {}),
  }
}

export class AnthropicProvider implements LLMProvider {
  private nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()
  /** Server-side context edits and the beta each needs. */
  private readonly nativeContextEdits: Array<{ beta: string; edit: Record<string, unknown> }> = []
  /** Threshold compaction with a pause: how many history messages stay verbatim behind the block. */
  private readonly compactionPauseKeep?: number
  private readonly onDemandCompactor?: OnDemandCompactor

  constructor(
    private readonly apiKey: string,
    private readonly model = "claude-sonnet-4-6",
    private readonly maxTokens = 8096,
    options: AnthropicProviderOptions = {},
  ) {
    // Clearing runs first so the compaction trigger reads the input after tool results are cleared.
    if (options.contextEditing) this.nativeContextEdits.push({ beta: CONTEXT_MANAGEMENT_BETA, edit: clearToolUsesEdit(options.contextEditing) })
    const compaction = options.compaction ? resolveCompaction(options.compaction) : undefined
    if (compaction?.mode === "threshold") {
      this.nativeContextEdits.push({ beta: THRESHOLD_COMPACTION_BETA, edit: compaction.edit })
      if (compaction.edit.pause_after_compaction) this.compactionPauseKeep = compaction.keepRecentMessages ?? 0
    } else if (compaction?.mode === "on_demand") {
      this.onDemandCompactor = new OnDemandCompactor(compaction.config, body => this.createMessage(body))
    }
  }

  /** On-demand compaction in flight settles; for tests and orderly shutdown. */
  compactionIdle(): Promise<void> {
    return this.onDemandCompactor?.idle() ?? Promise.resolve()
  }

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
      ...(this.nativeContextEdits.length || this.onDemandCompactor ? {
        nativeContext: {
          ...(this.nativeContextEdits.some(native => native.edit.type === "clear_tool_uses_20250919") ? { toolResultClearing: true } : {}),
          ...(this.onDemandCompactor || this.nativeContextEdits.some(native => native.edit.type === "compact_20260112") ? { compaction: true } : {}),
        },
      } : {}),
    }
  }

  requestPlanIdentity() {
    return {
      providerId: "anthropic",
      modelId: this.model,
      endpoint: { id: "anthropic.messages", protocol: "anthropic-messages" as const, baseURL: "https://api.anthropic.com" },
    }
  }

  peekProviderReplay(message: Pick<ModelMessage, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { protocol: "anthropic-messages", native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<ModelMessage, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.protocol === "anthropic-messages" && replay.native_blocks?.length) {
      this.nativeAssistantBlocks.set(assistantReplayKey(message), replay.native_blocks)
    }
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<ModelMessage> {
    return collectStreamMessage(this.stream(context, tools, extensions))
  }

  prepareRequest(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: ProviderRunState): PreparedProviderRequest {
    const strategy = resolveCacheBreakpointStrategy(extensions)
    const emitOnSystemBlocks = strategy === "default" || strategy === "system-only"
    const cc = { type: "ephemeral" as const }
    const systemBlocks: Array<{ type: "text"; text: string; cache_control?: { type: "ephemeral" } }> = []
    if (context.systemStable) {
      systemBlocks.push({ type: "text", text: context.systemStable, ...(emitOnSystemBlocks ? { cache_control: cc } : {}) })
    }
    if (context.systemKnowledge) {
      systemBlocks.push({ type: "text", text: context.systemKnowledge, ...(emitOnSystemBlocks ? { cache_control: cc } : {}) })
    }
    const system = systemBlocks.length ? systemBlocks : (context.systemText || undefined)
    const replay = (message: ModelMessage) => this.nativeAssistantBlocks.get(assistantReplayKey(message))
    let msgs: Array<Record<string, unknown>> = []
    // For each wire message, the index of the rendered turn it came from; the state turn is -1.
    let wireTurns: number[] = []
    context.turns.forEach((turn, index) => {
      for (const message of toAnthropicMessages({ systemText: "", turns: [turn] }, replay)) {
        msgs.push(message)
        wireTurns.push(index)
      }
    })
    // The API ignores everything before a compaction block, so it is not sent.
    let head = -1
    for (let index = msgs.length - 1; index >= 0 && head < 0; index--) {
      const content = msgs[index].content
      if (Array.isArray(content) && (content as Array<Record<string, unknown>>).some(block => block.type === "compaction")) head = index
    }
    if (head > 0) {
      msgs = msgs.slice(head)
      wireTurns = wireTurns.slice(head)
    }
    applyMessageCacheControl(
      msgs,
      typeof context.frozenPrefixLen === "number" && head > 0 ? context.frozenPrefixLen - head : context.frozenPrefixLen,
      strategy,
    )
    // Append the volatile State turn AFTER the cache breakpoints (uncached tail);
    // absent on un-rebuilt bindings, where the state is already inside `turns`.
    // Render through toAnthropicMessages so assistant tool_use blocks are
    // serialized correctly — raw content would silently drop toolCalls.
    if (context.stateTurn) {
      const stateCtx: RenderedContext = { systemText: "", turns: [context.stateTurn] }
      const stateMsgs = toAnthropicMessages(stateCtx, replay)
      msgs.push(...stateMsgs)
      wireTurns.push(...stateMsgs.map(() => -1))
    }
    assertCacheBudget(system, tools.length)

    const builtTools = tools.length ? buildAnthropicTools(tools, !Array.isArray(system), strategy) : undefined
    // I1: capture which slots will carry cache_control for pro-rata attribution at usage time.
    const slotBp = countCacheControlSlots(system, builtTools, msgs)
    const body: Record<string, unknown> = {
      model: this.model,
      max_tokens: this.maxTokens,
      messages: msgs,
      stream: true,
      ...(system ? { system } : {}),
      ...(builtTools ? { tools: builtTools } : {}),
    }
    if (extensions?.enable_thinking) {
      body.thinking = { type: "enabled", budget_tokens: 8000 }
    }
    // The kernel stopped running these tiers on the strength of the edits, so they ride on every
    // request beside any betas or edits the caller passed.
    if (this.nativeContextEdits.length) {
      const betas = Array.isArray(extensions?.betas) ? extensions.betas.filter((value): value is string => typeof value === "string") : []
      const callerManagement = extensions?.context_management && typeof extensions.context_management === "object"
        ? extensions.context_management as { edits?: unknown[] }
        : {}
      const edits = Array.isArray(callerManagement.edits) ? [...callerManagement.edits] : []
      for (const native of this.nativeContextEdits) {
        if (!betas.includes(native.beta)) betas.push(native.beta)
        if (!edits.some(edit => (edit as { type?: unknown })?.type === native.edit.type)) edits.push(native.edit)
      }
      body.betas = betas
      body.context_management = { ...callerManagement, edits }
    }
    this.onDemandCompactor?.apply(body)

    const frozenBody = requestSnapshot(body)
    return {
      scope: "encoded_body", request: requestSnapshot(frozenBody), state: requestSnapshot(state ?? null),
      stream: signal => this.streamTurn(context, frozenBody, wireTurns, slotBp, signal),
    }
  }

  /** One turn: the response's events, or — when a compaction pause stops the response at the
   *  summary — the continuation's. With a pause configured, events are held until the response
   *  shows it is not a pause (any non-usage event) or ends. */
  private async *streamTurn(
    context: RenderedContext,
    body: Record<string, unknown>,
    wireTurns: number[],
    slotBp: ReturnType<typeof countCacheControlSlots>,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const first = this.streamBody(body, slotBp, signal)[Symbol.asyncIterator]()
    let held: StreamEvent[] | undefined = this.compactionPauseKeep !== undefined ? [] : undefined
    let outcome: StreamOutcome
    for (;;) {
      const next = await first.next()
      if (next.done) { outcome = next.value; break }
      if (held && next.value.type !== "usage") {
        yield* held
        held = undefined
      }
      if (held) held.push(next.value)
      else yield next.value
    }
    const paused = held && outcome.stopReason === "compaction" ? compactionBlock(outcome.nativeBlocks) : undefined
    let unanchoredBlock: Record<string, unknown> | undefined
    if (paused) {
      const resumed = resumeAfterCompactionPause(
        body.messages as Array<Record<string, unknown>>,
        historyCount(wireTurns),
        paused,
        this.compactionPauseKeep,
      )
      if (resumed.anchor) {
        const turn = context.turns[wireTurns[resumed.anchor.wireIndex]]
        if (turn) this.nativeAssistantBlocks.set(assistantReplayKey(turn), resumed.anchor.content)
      } else {
        unanchoredBlock = paused
      }
      const continuation = this.streamBody({ ...withoutCompactionPause(body), messages: resumed.messages }, slotBp, signal)[Symbol.asyncIterator]()
      for (;;) {
        const next = await continuation.next()
        if (next.done) { outcome = next.value; break }
        yield next.value
      }
    } else if (held) {
      yield* held
    }
    this.rememberNativeBlocks(
      { content: outcome.finalText, toolCalls: outcome.finalToolCalls },
      [...(unanchoredBlock ? [unanchoredBlock] : []), ...outcome.nativeBlocks],
    )
    this.onDemandCompactor?.observe(body, historyCount(wireTurns), outcome.totalTokens)
  }

  private headers(body: Record<string, unknown>): Record<string, string> {
    return {
      "x-api-key": this.apiKey,
      "anthropic-version": "2023-06-01",
      "content-type": "application/json",
      "anthropic-beta": [
        PROMPT_CACHING_BETA,
        ...(Array.isArray(body.betas) ? body.betas.filter((value): value is string => typeof value === "string" && value !== PROMPT_CACHING_BETA) : []),
      ].filter((value, index, values) => values.indexOf(value) === index).join(","),
    }
  }

  /** A non-streaming request; the on-demand compaction request goes this way. */
  private async createMessage(body: Record<string, unknown>): Promise<Record<string, unknown>> {
    const resp = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: this.headers(body),
      // Betas travel in the header; the Messages API refuses them as a body field.
      body: JSON.stringify({ ...body, stream: undefined, betas: undefined }),
    })
    if (!resp.ok) {
      throw Object.assign(new Error(`Anthropic ${resp.status}: ${await resp.text()}`), { status: resp.status })
    }
    return await resp.json() as Record<string, unknown>
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, state?: ProviderRunState, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    yield* this.prepareRequest(context, tools, extensions, state).stream(signal)
  }

  private async *streamBody(
    body: Record<string, unknown>,
    slotBp: ReturnType<typeof countCacheControlSlots>,
    signal?: AbortSignal,
  ): AsyncGenerator<StreamEvent, StreamOutcome> {
    const resp = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: this.headers(body),
      // Betas travel in the header; the Messages API refuses them as a body field.
      body: JSON.stringify({ ...body, betas: undefined }),
      ...(signal ? { signal } : {}), // #2-B-ii: a preempt aborts the in-flight request at the socket.
    })
    if (!resp.ok) {
      throw Object.assign(new Error(`Anthropic ${resp.status}: ${await resp.text()}`), { status: resp.status })
    }

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
    let stopReason: string | undefined
    const outcome = (): StreamOutcome => ({
      nativeBlocks: Object.keys(nativeBlocks).map(Number).sort((a, b) => a - b).map(index => nativeBlocks[index]),
      finalText,
      finalToolCalls,
      ...(stopReason ? { stopReason } : {}),
      totalTokens: uncachedInput + cacheReadTokens + cacheCreationTokens + outputTokens,
    })

    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split("\n")
      buf = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.startsWith("data: ")) continue
        const data = line.slice(6).trim()
        if (data === "[DONE]") return outcome()
        try {
          const evt = JSON.parse(data) as Record<string, unknown>
          if (evt.type === "message_start" || evt.type === "message_delta") {
            // stop_reason rides on message_delta; "max_tokens" drives output-cap recovery and
            // "compaction" marks a paused threshold compaction.
            const eventStopReason = (evt as { delta?: { stop_reason?: string | null } }).delta?.stop_reason ?? undefined
            if (eventStopReason) stopReason = eventStopReason
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
                const bySlot = estimateCacheReadBySlot(cacheReadTokens, slotBp)
                yield {
                  type: "usage",
                  totalTokens: inputTokens + outputTokens,
                  inputTokens,
                  outputTokens,
                  cacheReadInputTokens: cacheReadTokens,
                  cacheCreationInputTokens: cacheCreationTokens,
                  ...(bySlot ? { cacheReadInputTokensBySlot: bySlot } : {}),
                  ...(eventStopReason ? { stopReason: eventStopReason } : {}),
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
            } else if (d.type === "compaction_delta") {
              // The summary arrives whole in one delta; it is replayed, never shown.
              nativeBlocks[idx] = { ...nativeBlocks[idx], content: String(nativeBlocks[idx]?.content ?? "") + String(d.content ?? "") }
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
              finalToolCalls.push({ id: tb.id, name: tb.name, arguments: toolArgumentsText(tb.argsBuf) })
              yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args, ...malformedToolArguments(tb.argsBuf) } as ToolCallEvent
            }
          }
        } catch { /* skip malformed */ }
      }
    }

    return outcome()
  }

  private rememberNativeBlocks(
    message: Pick<ModelMessage, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!blocks.length) return
    if (!message.toolCalls?.length && !blocks.some(b => b.type === "thinking" || b.type === "compaction")) return
    this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }
}
