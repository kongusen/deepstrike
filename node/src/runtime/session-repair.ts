import type { ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"
import { sanitizeReplayText } from "./replay-sanitize.js"

export { REPLAY_CONTENT_MAX_BYTES as RECOVERY_CONTENT_MAX_BYTES } from "./replay-sanitize.js"

function estimateTokenCount(text: string): number {
  return Math.max(1, Math.ceil(text.length / 4))
}

/**
 * Normalize a persisted llm_completed event for recovery.
 *
 * Content is sanitized and token_count backfilled, but the stored
 * `provider_replay` envelope is passed through verbatim — this layer is
 * provider-neutral and must never synthesize protocol-specific replay shapes
 * (e.g. Anthropic `native_blocks`). Legacy reconstruction for a given protocol
 * is the responsibility of that provider's `seedProviderReplay`.
 */
export function normalizeLlmCompleted(
  event: Extract<SessionEvent, { kind: "llm_completed" }>,
  maxBytes?: number,
): Extract<SessionEvent, { kind: "llm_completed" }> {
  const content = sanitizeReplayText(event.content ?? "", maxBytes)
  const toolCalls = event.tool_calls ?? []
  const providerReplay = event.provider_replay
  return {
    kind: "llm_completed",
    turn: event.turn,
    content,
    tool_calls: toolCalls,
    token_count: event.token_count ?? estimateTokenCount(content),
    ...(providerReplay ? { provider_replay: providerReplay } : {}),
  }
}

/** Repair event log for recovery minimum set before preload/wake. */
export function repairEventsForRecovery(
  events: Array<{ seq: number; event: SessionEvent }>,
  maxBytes?: number,
): Array<{ seq: number; event: SessionEvent }> {
  return events.map(entry => {
    if (entry.event.kind !== "llm_completed") return entry
    return { ...entry, event: normalizeLlmCompleted(entry.event, maxBytes) }
  })
}

/** Build a complete llm_completed payload for SessionLog append. */
export function buildLlmCompletedEvent(input: {
  turn: number
  content: string
  tokenCount?: number
  toolCalls: ToolCall[]
  providerReplay?: ProviderReplay
}): Extract<SessionEvent, { kind: "llm_completed" }> {
  return normalizeLlmCompleted({
    kind: "llm_completed",
    turn: input.turn,
    content: sanitizeReplayText(input.content),
    tool_calls: input.toolCalls ?? [],
    token_count: input.tokenCount,
    provider_replay: input.providerReplay,
  })
}

/** Build run_terminal with required recovery fields. */
export function buildRunTerminalEvent(input: {
  reason: string
  turnsUsed: number
  totalTokens: number
}): Extract<SessionEvent, { kind: "run_terminal" }> {
  return {
    kind: "run_terminal",
    reason: input.reason,
    turns_used: Math.max(0, input.turnsUsed),
    total_tokens: Math.max(0, input.totalTokens),
  }
}
