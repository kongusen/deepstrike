import type { ProviderMessage, ProviderReplay, ProviderWireEvidence, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"
import type { WorkflowNodeStatus } from "./types/agent.js"
import { sanitizeReplayText } from "./replay-sanitize.js"

export { REPLAY_CONTENT_MAX_BYTES as RECOVERY_CONTENT_MAX_BYTES } from "./replay-sanitize.js"

/**
 * Normalize a persisted llm_completed event for recovery. Content is sanitized
 * and any existing token_count remains raw evidence, but the stored `provider_replay` envelope is
 * passed through verbatim — this layer is provider-neutral and never
 * synthesizes protocol-specific replay shapes. Canonical replay seeding is the
 * responsibility of the target provider's `seedProviderReplay`.
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
    ...(event.token_count !== undefined ? { token_count: event.token_count } : {}),
    ...(providerReplay ? { provider_replay: providerReplay } : {}),
    // P4 §3: the evidence-plane fields ride along verbatim — recovery never reads them
    // (SessionLog is evidence, not authority), so normalize must neither synthesize nor drop them.
    ...(event.effect_id !== undefined ? { effect_id: event.effect_id } : {}),
    ...(event.invocation_id !== undefined ? { invocation_id: event.invocation_id } : {}),
    ...(event.wire_evidence !== undefined ? { wire_evidence: event.wire_evidence } : {}),
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
  effectId?: string
  invocationId?: string
  wireEvidence?: ProviderWireEvidence
}): Extract<SessionEvent, { kind: "llm_completed" }> {
  return normalizeLlmCompleted({
    kind: "llm_completed",
    turn: input.turn,
    content: sanitizeReplayText(input.content),
    tool_calls: input.toolCalls ?? [],
    token_count: input.tokenCount,
    provider_replay: input.providerReplay,
    ...(input.effectId !== undefined ? { effect_id: input.effectId } : {}),
    ...(input.invocationId !== undefined ? { invocation_id: input.invocationId } : {}),
    ...(input.wireEvidence !== undefined ? { wire_evidence: input.wireEvidence } : {}),
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

/** Build the audit projection emitted after a workflow node finishes. */
export function buildWorkflowNodeCompletedEvent(input: {
  turn: number
  agentId: string
  status: WorkflowNodeStatus
  termination: string
  classifyBranch?: string
  tournamentWinner?: string
  loopContinue?: boolean
  output?: ProviderMessage
}): Extract<SessionEvent, { kind: "workflow_node_completed" }> {
  return {
    kind: "workflow_node_completed",
    turn: input.turn,
    agent_id: input.agentId,
    status: input.status,
    termination: input.termination,
    ...(input.classifyBranch !== undefined ? { classify_branch: input.classifyBranch } : {}),
    ...(input.tournamentWinner !== undefined ? { tournament_winner: input.tournamentWinner } : {}),
    ...(input.loopContinue !== undefined ? { loop_continue: input.loopContinue } : {}),
    ...(input.output ? { output: input.output } : {}),
  }
}

/** Build the audit projection emitted after a runtime workflow submission. */
export function buildWorkflowNodesSubmittedEvent(input: {
  turn: number
  nodes: Record<string, unknown>[]
  baseIndex?: number
  submitterAgentId?: string
}): Extract<SessionEvent, { kind: "workflow_nodes_submitted" }> {
  return {
    kind: "workflow_nodes_submitted",
    turn: input.turn,
    nodes: input.nodes,
    ...(input.baseIndex !== undefined ? { base_index: input.baseIndex } : {}),
    ...(input.submitterAgentId !== undefined ? { submitter_agent_id: input.submitterAgentId } : {}),
  }
}
