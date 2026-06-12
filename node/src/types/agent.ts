import type { Message, ToolSchema } from "../types.js"

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
  /** R3-1: nodes this node's agent asked to append to the parent workflow DAG (via the
   *  `submit_workflow_nodes` tool). Surfaced by the orchestrator; the `runWorkflow` driver sends
   *  them to the parent kernel before this node's completion. SDK-internal — not sent over the wire
   *  on the kernel `SubAgentResult` (see `subAgentResultToKernel`). */
  submittedNodes?: WorkflowNodeSpec[]
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

// ─── W0-ABI: declarative workflow specs ───

/** A task for a workflow node: a full object, or a bare goal string. */
export type WorkflowTaskSpec = { goal: string; criteria?: string[]; lane?: string } | string

/** W3 trust level for a workflow node. */
export type NodeTrust = "trusted" | "quarantined"

/** One node in a declarative workflow DAG (camelCase host shape). */
export interface WorkflowNodeSpec {
  task: WorkflowTaskSpec
  role: KernelAgentRole
  isolation?: AgentIsolation
  contextInheritance?: ContextInheritance
  modelHint?: string
  /** W3: `quarantined` nodes read untrusted content and must run without privileges. */
  trust?: NodeTrust
  /** G3: JSON Schema the node's output must conform to. The kernel carries it to the spawn
   *  descriptor; the runner instructs the agent and validates + retries once on mismatch. */
  outputSchema?: Record<string, unknown>
  /** G2: make this a deterministic *reduce* node — it runs no LLM agent. The runner routes it to the
   *  registered reducer of this name, over its `dependsOn` nodes' outputs (dedupe / filter / merge). */
  reducer?: string
  /** Indices of nodes this node depends on. */
  dependsOn?: number[]
}

/** A declarative workflow DAG the kernel runs node-by-node, gating each spawn. */
export interface WorkflowSpec {
  nodes: WorkflowNodeSpec[]
}

/** Per-node spawn descriptor carried in the `workflow_batch_spawned` observation. */
export interface WorkflowSpawnInfo {
  agent_id: string
  goal: string
  role: string
  isolation: string
  context_inheritance: string
  model_hint?: string
  /** W3 trust level: `"trusted"` | `"quarantined"`. */
  trust?: string
  /** G3: JSON Schema the node's output must conform to (carried verbatim from the spec). */
  output_schema?: Record<string, unknown>
  /** G2: for a reduce node, the name of the registered host function to run (no LLM). */
  reducer?: string
  /** G2: the dependency agent ids whose outputs a reduce node consumes. */
  input_agent_ids?: string[]
}

/** G4 budget-as-signal: the workflow's remaining headroom under the active quota, carried on the
 *  `workflow_batch_spawned` observation so a coordinator node can scale its next submission. */
export interface WorkflowBudget {
  nodes_used: number
  nodes_max?: number
  nodes_remaining?: number
  running_subagents: number
  max_concurrent_subagents?: number
  concurrency_remaining?: number
}

/** G4: a concise, human-readable budget note appended to a coordinator node's goal, so its agent can
 *  size a `submit_workflow_nodes` batch to what is actually available. Returns "" when nothing is
 *  bounded (no quota ⇒ no signal). */
export function workflowBudgetNote(budget: WorkflowBudget | undefined): string {
  if (!budget) return ""
  const parts: string[] = []
  if (budget.nodes_remaining != null && budget.nodes_max != null) {
    parts.push(`nodes ${budget.nodes_used}/${budget.nodes_max} used, ${budget.nodes_remaining} remaining`)
  }
  if (budget.concurrency_remaining != null && budget.max_concurrent_subagents != null) {
    parts.push(
      `concurrency ${budget.running_subagents}/${budget.max_concurrent_subagents} running, ${budget.concurrency_remaining} free`,
    )
  }
  if (parts.length === 0) return ""
  return (
    `[workflow budget] ${parts.join(" · ")}. ` +
    "If you submit more workflow nodes, keep the batch within the remaining node budget."
  )
}

/** Map one host `WorkflowNodeSpec` to its snake_case kernel JSON. Shared by `load_workflow` (the
 *  whole spec) and `submit_workflow_nodes` (R3-1 runtime append) so the two encodings never drift. */
export function workflowNodeSpecToKernel(n: WorkflowNodeSpec): Record<string, unknown> {
  const task = typeof n.task === "string" ? { goal: n.task } : n.task
  return {
    task: {
      goal: task.goal,
      // `criteria` is required by the kernel's RuntimeTask serde (no default).
      criteria: task.criteria ?? [],
      ...(task.lane ? { lane: task.lane } : {}),
    },
    role: n.role,
    // role/isolation/context_inheritance have no serde default in the kernel — always emit.
    isolation: n.isolation ?? "shared",
    context_inheritance: n.contextInheritance ?? "none",
    ...(n.modelHint ? { model_hint: n.modelHint } : {}),
    ...(n.trust && n.trust !== "trusted" ? { trust: n.trust } : {}),
    ...(n.outputSchema ? { output_schema: n.outputSchema } : {}),
    // G2: a reducer name lowers to the kernel's `NodeKind::Reduce` (serde-tagged by `type`).
    ...(n.reducer ? { kind: { type: "reduce", reducer: n.reducer } } : {}),
    ...(n.dependsOn && n.dependsOn.length ? { depends_on: n.dependsOn } : {}),
  }
}

/** Map a host `WorkflowSpec` to the snake_case kernel JSON (`load_workflow.spec`). */
export function workflowSpecToKernel(spec: WorkflowSpec): Record<string, unknown> {
  return { nodes: spec.nodes.map(workflowNodeSpecToKernel) }
}

/** R3-1: map a batch of host nodes to the `submit_workflow_nodes` kernel event body. G1: pass
 *  `submitterAgentId` (the node that requested the append) so the kernel can enforce no-privilege-
 *  escalation — a quarantined submitter's nodes are coerced to quarantined. Omitted ⇒ no coercion. */
export function submitWorkflowNodesToKernel(
  nodes: WorkflowNodeSpec[],
  submitterAgentId?: string,
): Record<string, unknown> {
  return {
    kind: "submit_workflow_nodes",
    nodes: nodes.map(workflowNodeSpecToKernel),
    ...(submitterAgentId ? { submitter_agent_id: submitterAgentId } : {}),
  }
}

/** R3-1: the tool a workflow-coordinator node's agent calls to append work to the running DAG
 *  (true loop-until-done / dynamic fan-out). Give it to nodes meant to fan out; the runner intercepts
 *  the call and routes the nodes to the parent kernel (the child's own kernel holds no workflow). */
export const submitWorkflowNodesTool: ToolSchema = {
  name: "submit_workflow_nodes",
  description:
    "Append new nodes to the running workflow DAG (dynamic fan-out / loop-until-done). Each node " +
    "spawns as a gated sub-agent. Use when you discover more work that should run as its own node.",
  parameters: JSON.stringify({
    type: "object",
    properties: {
      nodes: {
        type: "array",
        description: "Workflow nodes to append; each runs as a gated sub-agent.",
        items: {
          type: "object",
          properties: {
            task: {
              description: "The node's goal: a string, or an object { goal, criteria?, lane? }.",
              oneOf: [
                { type: "string" },
                {
                  type: "object",
                  properties: {
                    goal: { type: "string" },
                    criteria: { type: "array", items: { type: "string" } },
                  },
                  required: ["goal"],
                },
              ],
            },
            role: { type: "string", enum: ["explore", "plan", "implement", "verify", "custom"] },
            isolation: { type: "string", enum: ["shared", "read_only", "worktree", "remote"] },
            contextInheritance: { type: "string", enum: ["none", "system_only", "full"] },
            trust: { type: "string", enum: ["trusted", "quarantined"] },
            outputSchema: {
              type: "object",
              description: "Optional JSON Schema the node's output must conform to (validated + retried SDK-side).",
            },
            reducer: {
              type: "string",
              description: "Make this a deterministic reduce node (no LLM); names a registered reducer.",
            },
            dependsOn: {
              type: "array",
              items: { type: "integer" },
              description: "Batch-relative, backward-only dependency indices within this submission.",
            },
          },
          required: ["task", "role"],
        },
      },
    },
    required: ["nodes"],
  }),
}

/** Build a sub-agent run spec for a kernel-generated workflow node. */
export function workflowNodeToSpec(node: WorkflowSpawnInfo, parentSessionId: string): AgentRunSpec {
  return {
    identity: {
      agentId: node.agent_id,
      sessionId: `${parentSessionId}-${node.agent_id}`,
      isSubAgent: true,
      parentSessionId,
    },
    role: node.role as KernelAgentRole,
    isolation: node.isolation as AgentIsolation,
    goal: node.goal,
  }
}

/** Build the host manifest for a kernel-generated workflow node. */
export function workflowNodeToManifest(
  node: WorkflowSpawnInfo,
  parentSessionId: string,
): AgentProcessChangedObservation {
  return {
    kind: "agent_process_changed",
    agent_id: node.agent_id,
    parent_session_id: parentSessionId,
    role: node.role,
    isolation: node.isolation,
    context_inheritance: node.context_inheritance,
  }
}

// ─── W1/W2 workflow templates (the six patterns as one-liners) ───
// Roles carry the kernel's role_defaults isolation/inheritance so host-built specs match the
// core `orchestration::workflow` constructors (e.g. verifiers stay bias-resistant).

function asTask(t: WorkflowTaskSpec): { goal: string; criteria?: string[]; lane?: string } {
  return typeof t === "string" ? { goal: t } : t
}

/** N parallel read-only Explore workers feeding a single Plan synthesizer (barrier). */
export function fanoutSynthesize(workers: WorkflowTaskSpec[], synthesize: WorkflowTaskSpec): WorkflowSpec {
  const nodes: WorkflowNodeSpec[] = workers.map(t => ({
    task: asTask(t),
    role: "explore",
    isolation: "read_only",
    contextInheritance: "system_only",
  }))
  nodes.push({
    task: asTask(synthesize),
    role: "plan",
    isolation: "shared",
    contextInheritance: "full",
    dependsOn: workers.map((_, i) => i),
  })
  return { nodes }
}

/** N parallel Implement generators feeding a single Verify filter/dedupe step (barrier). */
export function generateAndFilter(generators: WorkflowTaskSpec[], filter: WorkflowTaskSpec): WorkflowSpec {
  const nodes: WorkflowNodeSpec[] = generators.map(t => ({
    task: asTask(t),
    role: "implement",
    isolation: "worktree",
    contextInheritance: "full",
  }))
  nodes.push({
    task: asTask(filter),
    role: "verify",
    isolation: "read_only",
    contextInheritance: "none",
    dependsOn: generators.map((_, i) => i),
  })
  return { nodes }
}

/**
 * One fresh-context verifier per rule/claim (parallel) + optional skeptic that depends on all and
 * re-checks flags. Verifiers run read-only with no inherited author context (bias-resistant).
 */
export function verifyRules(rules: WorkflowTaskSpec[], skeptic?: WorkflowTaskSpec): WorkflowSpec {
  const nodes: WorkflowNodeSpec[] = rules.map(t => ({
    task: asTask(t),
    role: "verify",
    isolation: "read_only",
    contextInheritance: "none",
  }))
  if (skeptic !== undefined) {
    nodes.push({
      task: asTask(skeptic),
      role: "verify",
      isolation: "read_only",
      contextInheritance: "none",
      dependsOn: rules.map((_, i) => i),
    })
  }
  return { nodes }
}
