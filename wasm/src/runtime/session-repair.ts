import type { ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"
import { sanitizeReplayText } from "./replay-sanitize.js"

export { REPLAY_CONTENT_MAX_BYTES as RECOVERY_CONTENT_MAX_BYTES } from "./replay-sanitize.js"

function estimateTokenCount(text: string): number {
  return Math.max(1, Math.ceil(text.length / 4))
}

function parseToolInput(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

export function synthesizeProviderReplay(
  content: string,
  toolCalls: ToolCall[],
): ProviderReplay | undefined {
  if (!toolCalls.length) return undefined
  const blocks: Array<Record<string, unknown>> = []
  if (content) blocks.push({ type: "text", text: content })
  for (const tc of toolCalls) {
    blocks.push({
      type: "tool_use",
      id: tc.id,
      name: tc.name,
      input: parseToolInput(tc.arguments),
    })
  }
  return { native_blocks: blocks }
}

export function effectiveProviderReplay(
  content: string,
  toolCalls: ToolCall[],
  stored?: ProviderReplay,
): ProviderReplay | undefined {
  if (stored?.native_blocks?.length || stored?.reasoning_content != null) {
    return stored
  }
  return synthesizeProviderReplay(content, toolCalls)
}

export function normalizeLlmCompleted(
  event: Extract<SessionEvent, { kind: "llm_completed" }>,
  maxBytes?: number,
): Extract<SessionEvent, { kind: "llm_completed" }> {
  const content = sanitizeReplayText(event.content ?? "", maxBytes)
  const toolCalls = event.tool_calls ?? []
  const providerReplay = effectiveProviderReplay(content, toolCalls, event.provider_replay)
  return {
    kind: "llm_completed",
    turn: event.turn,
    content,
    tool_calls: toolCalls,
    token_count: event.token_count ?? estimateTokenCount(content),
    ...(providerReplay ? { provider_replay: providerReplay } : {}),
  }
}

export function repairEventsForRecovery(
  events: Array<{ seq: number; event: SessionEvent }>,
  maxBytes?: number,
): Array<{ seq: number; event: SessionEvent }> {
  return events.map(entry => {
    if (entry.event.kind !== "llm_completed") return entry
    return { ...entry, event: normalizeLlmCompleted(entry.event, maxBytes) }
  })
}

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
