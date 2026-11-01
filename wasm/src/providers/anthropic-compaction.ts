// Server-side compaction for the Anthropic provider, in its two forms. Threshold compaction
// (`compact_20260112`) rides on every request and the API summarizes once input reaches the
// trigger; with `pauseAfterCompaction` the API stops after writing the summary so the provider can
// keep the newest turns verbatim behind it. On-demand compaction (`compact-2026-09-04`) is a
// separate request the provider sends in the background once a turn's input passes a limit; the
// signed block it returns replaces the messages it summarized on every later request. Either way
// the kernel sees an ordinary turn: the provider owns the block, and the kernel's renewal still
// decides on the full local history. Mirrors node/src/providers/anthropic-compaction.ts.

export const THRESHOLD_COMPACTION_BETA = "compact-2026-01-12"
export const ON_DEMAND_COMPACTION_BETA = "compact-2026-09-04"

/** `compact_20260112` settings. Unset fields take the API defaults. */
export interface AnthropicThresholdCompaction {
  mode?: "threshold"
  /** Input tokens at which the API summarizes earlier history (at least 50,000; API default 150,000). */
  triggerInputTokens?: number
  /** A summarization prompt that replaces the API's default one. */
  instructions?: string
  /** Stop after the summary is written, then continue with the newest history messages kept
   *  verbatim behind it. Their thinking blocks are dropped: they were produced with the full
   *  history in view and would fail the API's prefix check. */
  pauseAfterCompaction?: { keepRecentMessages: number }
}

/** On-demand compaction settings (`compact-2026-09-04`). */
export interface AnthropicOnDemandCompaction {
  mode: "on_demand"
  /** Once a turn's input plus output reaches this many tokens, the provider asks for a summary of
   *  that turn's history in the background and adopts it on the first request after it arrives. */
  compactAtInputTokens: number
  /** A summarization prompt that replaces the API's default one (at most 16,384 characters). */
  instructions?: string
  /** `max_tokens` for the summarization call; defaults to the turn's own. */
  maxTokens?: number
}

export type AnthropicCompaction = AnthropicThresholdCompaction | AnthropicOnDemandCompaction

export type ResolvedCompaction =
  | { mode: "threshold"; edit: Record<string, unknown>; keepRecentMessages?: number }
  | { mode: "on_demand"; config: AnthropicOnDemandCompaction }

type WireMessage = Record<string, unknown>
type WireBlock = Record<string, unknown>

function positiveInteger(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new TypeError(`AnthropicProvider compaction.${field} must be a positive integer`)
  }
  return value
}

export function resolveCompaction(config: true | AnthropicCompaction): ResolvedCompaction {
  const c: AnthropicCompaction = config === true ? {} : config
  if (c.mode === "on_demand") {
    positiveInteger(c.compactAtInputTokens, "compactAtInputTokens")
    if (c.maxTokens !== undefined) positiveInteger(c.maxTokens, "maxTokens")
    for (const field of ["triggerInputTokens", "pauseAfterCompaction"]) {
      if ((c as unknown as Record<string, unknown>)[field] !== undefined) {
        throw new TypeError(`AnthropicProvider compaction.${field} applies to threshold compaction, not on_demand`)
      }
    }
    return { mode: "on_demand", config: c }
  }
  if (c.mode !== undefined && c.mode !== "threshold") {
    throw new TypeError(`AnthropicProvider compaction.mode must be "threshold" or "on_demand", got ${String(c.mode)}`)
  }
  const keep = c.pauseAfterCompaction?.keepRecentMessages
  if (c.pauseAfterCompaction !== undefined && (typeof keep !== "number" || !Number.isSafeInteger(keep) || keep < 0)) {
    throw new TypeError("AnthropicProvider compaction.pauseAfterCompaction.keepRecentMessages must be a non-negative integer")
  }
  return {
    mode: "threshold",
    edit: {
      type: "compact_20260112",
      ...(c.triggerInputTokens !== undefined ? { trigger: { type: "input_tokens", value: c.triggerInputTokens } } : {}),
      ...(c.pauseAfterCompaction !== undefined ? { pause_after_compaction: true } : {}),
      ...(c.instructions !== undefined ? { instructions: c.instructions } : {}),
    },
    ...(keep !== undefined ? { keepRecentMessages: keep } : {}),
  }
}

/** The request that continues a paused threshold compaction: the same edits without the pause. */
export function withoutCompactionPause(params: Record<string, unknown>): Record<string, unknown> {
  const management = params.context_management as { edits?: WireBlock[] } | undefined
  if (!Array.isArray(management?.edits)) return params
  return {
    ...params,
    context_management: {
      ...management,
      edits: management.edits.map(edit => {
        if (edit?.type !== "compact_20260112") return edit
        const { pause_after_compaction: _pause, ...rest } = edit
        return rest
      }),
    },
  }
}

export function compactionBlock(content: unknown): WireBlock | undefined {
  return Array.isArray(content)
    ? (content as WireBlock[]).find(block => block?.type === "compaction")
    : undefined
}

function contentBlocks(message: WireMessage): WireBlock[] {
  const content = message.content
  if (typeof content === "string") return content ? [{ type: "text", text: content }] : []
  return Array.isArray(content) ? content as WireBlock[] : []
}

function withoutCacheControl(block: WireBlock): WireBlock {
  const { cache_control: _cache, ...rest } = block
  return rest
}

export interface CompactionResume {
  /** The messages of the continuation request. */
  messages: WireMessage[]
  /** When recent turns were kept: the wire index of the assistant message the block now heads,
   *  and that message's new content (the block, then its own blocks without thinking). */
  anchor?: { wireIndex: number; content: WireBlock[] }
}

/** How a paused threshold compaction continues. With `keepRecentMessages` the block heads the
 *  nearest assistant message at or before the first kept history message, so the kept turns follow
 *  the summary verbatim; without it (or when no assistant message qualifies) the block is
 *  appended as an assistant turn of its own, as the API's pause flow does. `historyCount` is the
 *  number of leading wire messages that are history (the volatile state turn follows them). */
export function resumeAfterCompactionPause(
  messages: WireMessage[],
  historyCount: number,
  block: WireBlock,
  keepRecentMessages: number | undefined,
): CompactionResume {
  const keep = keepRecentMessages ?? 0
  if (keep > 0) {
    const firstKept = historyCount - keep
    for (let index = Math.min(firstKept, historyCount - 1); index > 0; index--) {
      if (messages[index]?.role !== "assistant") continue
      const own = contentBlocks(messages[index])
        .filter(item => item.type !== "thinking" && item.type !== "redacted_thinking")
        .map(withoutCacheControl)
      const content = [block, ...own]
      return {
        messages: [{ role: "assistant", content }, ...messages.slice(index + 1)],
        anchor: { wireIndex: index, content },
      }
    }
  }
  return { messages: [...messages, { role: "assistant", content: [block] }] }
}

/** Wire values as the API compares them: `cache_control` is a request annotation, not content,
 *  and a string message body is the same content as one text block (a cache marker turns the one
 *  into the other). */
function stableJson(value: unknown): string {
  return JSON.stringify(value, (key, item) => {
    if (key === "cache_control") return undefined
    if (item && typeof item === "object" && !Array.isArray(item)
      && typeof (item as WireMessage).role === "string" && typeof (item as WireMessage).content === "string") {
      return { ...item, content: contentBlocks(item as WireMessage) }
    }
    return item
  })
}

interface OnDemandAdoption {
  /** The summarized messages, one digest each, as the compaction request carried them. */
  summarized: string[]
  /** The system prompt and tools the compaction request ran under. */
  frame: string
  /** The assistant message that stands in for the summarized messages. */
  message: WireMessage
}

/** The compaction request a turn's request becomes: the same model, system, tools and thinking,
 *  its history messages only, and none of the fields the API rejects on a summarization call. */
export function onDemandCompactionBody(
  sent: Record<string, unknown>,
  historyCount: number,
  config: AnthropicOnDemandCompaction,
): Record<string, unknown> {
  const {
    stream: _stream,
    context_management: _management,
    stop_sequences: _stop,
    ...body
  } = sent
  const choice = body.tool_choice as { type?: unknown } | undefined
  if (choice?.type === "any" || choice?.type === "tool") delete body.tool_choice
  const output = body.output_config as Record<string, unknown> | undefined
  if (output) {
    const { format: _format, task_budget: budget, ...rest } = output
    const taskBudget = budget && typeof budget === "object"
      ? (({ remaining: _remaining, ...kept }) => kept)(budget as Record<string, unknown>)
      : undefined
    const next = { ...rest, ...(taskBudget ? { task_budget: taskBudget } : {}) }
    if (Object.keys(next).length) body.output_config = next
    else delete body.output_config
  }
  const betas = (Array.isArray(sent.betas) ? sent.betas as string[] : [])
    .filter(beta => beta !== THRESHOLD_COMPACTION_BETA)
  if (!betas.includes(ON_DEMAND_COMPACTION_BETA)) betas.push(ON_DEMAND_COMPACTION_BETA)
  return {
    ...body,
    messages: (sent.messages as WireMessage[]).slice(0, historyCount),
    ...(config.maxTokens !== undefined ? { max_tokens: config.maxTokens } : {}),
    compaction: {
      type: "summarize",
      ...(config.instructions?.trim() ? { instructions: config.instructions } : {}),
    },
    betas,
  }
}

/** Owns on-demand compaction for one provider: decides when to ask for a summary, runs the
 *  request in the background, and swaps the block in for the messages it summarized. Adoptions
 *  chain — a later summary covers the earlier block — and every request re-derives the swap from
 *  the full history the kernel rendered, so a history the summary no longer matches (a renewal, a
 *  changed system prompt or toolset) is sent whole and the stale adoptions are dropped. State is
 *  in memory: after a restart the first turn past the limit compacts again. */
export class OnDemandCompactor {
  private adoptions: OnDemandAdoption[] = []
  private pending?: { settled?: OnDemandAdoption | null; done: Promise<void> }

  constructor(
    private readonly config: AnthropicOnDemandCompaction,
    private readonly send: (body: Record<string, unknown>) => Promise<Record<string, unknown>>,
  ) {}

  /** Swap the adopted summaries into a request's messages. Mutates `params`; returns whether a
   *  block was swapped in (the request then needs the on-demand beta). */
  apply(params: Record<string, unknown>): boolean {
    if (this.pending?.settled !== undefined) {
      if (this.pending.settled) this.adoptions.push(this.pending.settled)
      this.pending = undefined
    }
    if (!this.adoptions.length) return false
    const frame = stableJson([params.system ?? null, params.tools ?? null])
    let messages = params.messages as WireMessage[]
    let applied = 0
    for (const adoption of this.adoptions) {
      if (adoption.frame !== frame || adoption.summarized.length > messages.length) break
      const matches = adoption.summarized.every((digest, index) => stableJson(messages[index]) === digest)
      if (!matches) break
      messages = [adoption.message, ...messages.slice(adoption.summarized.length)]
      applied++
    }
    this.adoptions.length = applied
    if (!applied) return false
    params.messages = messages
    const betas = Array.isArray(params.betas) ? [...params.betas as string[]] : []
    if (!betas.includes(ON_DEMAND_COMPACTION_BETA)) betas.push(ON_DEMAND_COMPACTION_BETA)
    params.betas = betas
    return true
  }

  /** After a turn completes: once its input plus output reaches the limit, summarize the history
   *  that turn sent, unless a summary is already being written. Never throws; a summary that does
   *  not come back leaves the history as it is, and the next turn past the limit asks again. */
  observe(sent: Record<string, unknown>, historyCount: number, totalTokens: number): void {
    if (this.pending || totalTokens < this.config.compactAtInputTokens || historyCount <= 0) return
    const body = onDemandCompactionBody(sent, historyCount, this.config)
    const summarized = (body.messages as WireMessage[]).map(stableJson)
    const frame = stableJson([body.system ?? null, body.tools ?? null])
    const pending: { settled?: OnDemandAdoption | null; done: Promise<void> } = { done: Promise.resolve() }
    pending.done = this.send(body).then(raw => {
      const block = raw.stop_reason === "compaction" ? compactionBlock(raw.content) : undefined
      pending.settled = block ? { summarized, frame, message: { role: "assistant", content: [block] } } : null
    }, () => {
      pending.settled = null
    })
    this.pending = pending
  }

  /** Resolves once any summary in flight has settled. For tests and orderly shutdown. */
  async idle(): Promise<void> {
    await this.pending?.done
  }
}
