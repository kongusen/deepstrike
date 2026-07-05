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

/** Build workflow_node_completed for persistence after a node finishes. */
export function buildWorkflowNodeCompletedEvent(input: {
  turn: number
  agentId: string
  termination: string
}): Extract<SessionEvent, { kind: "workflow_node_completed" }> {
  return {
    kind: "workflow_node_completed",
    turn: input.turn,
    agent_id: input.agentId,
    termination: input.termination,
  }
}

/**
 * Recover completed workflow node agent_ids from a session event stream.
 * Scans for workflow_node_completed events and returns the agent_ids whose
 * termination was "completed". Used to rebuild resumedCompleted for resumeWorkflow.
 */
export function recoverCompletedWorkflowNodes(
  events: Array<{ seq: number; event: SessionEvent }>,
): string[] {
  const completed: string[] = []
  for (const { event } of events) {
    if (event.kind === "workflow_node_completed" && event.termination === "completed") {
      completed.push(event.agent_id)
    }
  }
  return completed
}

/** R3-1: build workflow_nodes_submitted for persistence after a runtime submission, so resume can
 *  re-apply it. `nodes` is the kernel-shape (snake_case) submitted node array. */
export function buildWorkflowNodesSubmittedEvent(input: {
  turn: number
  nodes: Record<string, unknown>[]
  baseIndex?: number
}): Extract<SessionEvent, { kind: "workflow_nodes_submitted" }> {
  return {
    kind: "workflow_nodes_submitted",
    turn: input.turn,
    nodes: input.nodes,
    ...(input.baseIndex !== undefined ? { base_index: input.baseIndex } : {}),
  }
}

/** R3-1: recover the runtime submission batches (in order) from a session event stream, to rebuild
 *  `resumed_submissions` for resumeWorkflow so dynamically-appended nodes are reconstructed. */
export function recoverSubmittedWorkflowNodes(
  events: Array<{ seq: number; event: SessionEvent }>,
): { submissions: Record<string, unknown>[][]; bases: number[] } {
  const submissions: Record<string, unknown>[][] = []
  const bases: number[] = []
  for (const { event } of events) {
    if (event.kind === "workflow_nodes_submitted") {
      submissions.push(event.nodes)
      // Absent on legacy logs → order-only replay (bases array stays parallel-short only
      // if ALL records carry it; a mixed log degrades to order-only for safety).
      if (event.base_index !== undefined) bases.push(event.base_index)
    }
  }
  return { submissions, bases: bases.length === submissions.length ? bases : [] }
}
