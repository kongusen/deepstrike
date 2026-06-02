import type { Message } from "../../types.js"

export type KernelAgentRole = "explore" | "plan" | "implement" | "verify" | "custom"
export type AgentIsolation = "shared" | "read_only" | "worktree" | "remote"
export type ContextInheritance = "none" | "system_only" | "full"
export type TerminationReason =
  | "completed"
  | "max_turns"
  | "token_budget"
  | "timeout"
  | "user_abort"
  | "error"
  | "milestone_exceeded"

export type MilestonePolicy = "require_verifier" | "terminate" | "auto_pass"

export interface AgentIdentity {
  agentId: string
  sessionId: string
  isSubAgent: boolean
  parentSessionId?: string
}

export interface AgentCapabilityFilter {
  allowedKinds?: string[]
  allowedIds?: string[]
}

export interface AgentRunSpec {
  identity: AgentIdentity
  role: KernelAgentRole
  isolation?: AgentIsolation
  goal: string
  verificationContractId?: string
  capabilityFilter?: AgentCapabilityFilter
  milestones?: MilestoneContract
  metadata?: Record<string, unknown>
}

/** Kernel process-table observation (Phase 3 canonical spawn signal). */
export interface AgentProcessChangedObservation {
  kind: "agent_process_changed"
  turn?: number
  agent_id: string
  parent_session_id: string
  role: string
  isolation: string
  context_inheritance: string
  state?: string
  permitted_capability_ids?: string[]
  result_termination?: string
}

/** Map kernel spawn observation → host manifest. */
export function spawnObservationToManifest(
  obs: AgentProcessChangedObservation | Record<string, unknown>,
  spec: AgentRunSpec,
  parentSessionId: string,
): AgentProcessChangedObservation {
  const o = obs as AgentProcessChangedObservation
  return {
    kind: "agent_process_changed",
    turn: o.turn,
    agent_id: String(o.agent_id ?? spec.identity.agentId),
    parent_session_id: String(o.parent_session_id ?? parentSessionId),
    role: String(o.role ?? spec.role),
    isolation: String(o.isolation ?? spec.isolation ?? "shared"),
    context_inheritance: String(o.context_inheritance ?? "none"),
    permitted_capability_ids: o.permitted_capability_ids ?? [],
  }
}

export function findSpawnProcessObservation(
  observations: Array<{ kind: string; agent_id?: string }>,
): AgentProcessChangedObservation | undefined {
  const hit = observations.find(
    o => o.kind === "agent_process_changed" && typeof o.agent_id === "string",
  )
  return hit as AgentProcessChangedObservation | undefined
}

export interface LoopResult {
  termination: TerminationReason | string
  finalMessage?: Message
  turnsUsed: number
  totalTokensUsed: number
}

export interface SubAgentResult {
  agentId: string
  result: LoopResult
}

export interface MilestoneCheckResult {
  phaseId: string
  passed: boolean
  reason?: string
}

export interface MilestonePhase {
  id: string
  criteria?: string[]
  unlocks?: Array<Record<string, unknown>>
  verifier?: Record<string, unknown>
  requiredEvidence?: string[]
}

export interface MilestoneContract {
  phases: MilestonePhase[]
}

export function agentIdentitySub(agentId: string, sessionId: string, parentSessionId?: string): AgentIdentity {
  return {
    agentId,
    sessionId,
    isSubAgent: true,
    ...(parentSessionId ? { parentSessionId } : {}),
  }
}

export function agentRunSpecToKernel(spec: AgentRunSpec): Record<string, unknown> {
  const out: Record<string, unknown> = {
    identity: {
      agent_id: spec.identity.agentId,
      session_id: spec.identity.sessionId,
      is_sub_agent: spec.identity.isSubAgent,
      ...(spec.identity.parentSessionId ? { parent_session_id: spec.identity.parentSessionId } : {}),
    },
    role: spec.role,
    isolation: spec.isolation ?? "shared",
    goal: spec.goal,
    capability_filter: {
      allowed_kinds: spec.capabilityFilter?.allowedKinds ?? [],
      allowed_ids: spec.capabilityFilter?.allowedIds ?? [],
    },
    metadata: spec.metadata ?? null,
  }
  if (spec.verificationContractId) out.verification_contract_id = spec.verificationContractId
  if (spec.milestones) out.milestones = milestoneContractToKernel(spec.milestones)
  return out
}

export function milestoneContractToKernel(contract: MilestoneContract): Record<string, unknown> {
  return {
    phases: contract.phases.map(phase => ({
      id: phase.id,
      criteria: phase.criteria ?? [],
      unlocks: phase.unlocks ?? [],
      ...(phase.verifier ? { verifier: phase.verifier } : {}),
      required_evidence: phase.requiredEvidence ?? [],
    })),
  }
}

export function milestoneCheckResultToKernel(result: MilestoneCheckResult): Record<string, unknown> {
  return {
    phase_id: result.phaseId,
    passed: result.passed,
    ...(result.reason ? { reason: result.reason } : {}),
  }
}

export function subAgentResultToKernel(result: SubAgentResult): Record<string, unknown> {
  const finalMessage = result.result.finalMessage
  return {
    agent_id: result.agentId,
    result: {
      termination: result.result.termination,
      final_message: finalMessage
        ? {
            role: finalMessage.role,
            content: finalMessage.content,
            tool_calls: (finalMessage.toolCalls ?? []).map(tc => ({
              id: tc.id,
              name: tc.name,
              arguments: JSON.parse(tc.arguments || "{}"),
            })),
            ...(finalMessage.tokenCount !== undefined ? { token_count: finalMessage.tokenCount } : {}),
          }
        : null,
      turns_used: result.result.turnsUsed,
      total_tokens_used: result.result.totalTokensUsed,
    },
  }
}

export function milestoneCheckPass(phaseId: string): MilestoneCheckResult {
  return { phaseId, passed: true }
}

export function milestoneCheckFail(phaseId: string, reason: string): MilestoneCheckResult {
  return { phaseId, passed: false, reason }
}
