import type { Message, ToolSchema } from "../types.js"
import { getKernel } from "../kernel.js"

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
  /** v0.2.35 recovery ladder: compaction exhausted and the prompt still exceeds the provider window. */
  | "context_overflow"
  /** Repeat-fuse escalation: the same tool call (name AND args) re-issued past `terminateAfter` —
   *  a stall, distinct from `max_turns` which productive runs can also hit. */
  | "no_progress"

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

export interface LoopRoundSpec {
  /** Hard round cap across the loop's lifetime; continue/sleep at the cap is coerced to stop. */
  maxRounds?: number
  /** Sleep clamp floor (ms). */
  minSleepMs?: number
  /** Sleep clamp ceiling (ms). */
  maxSleepMs?: number
  /** Fallback when a round ends without a pace call: "stop" (goal loops, default) | "sleep" (cron loops). */
  defaultAction?: "stop" | "sleep"
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
  /** ③ loop-agent rounds: presence makes this run ONE round of a paced loop (gates the
   *  kernel `pace` meta-tool and arms the pacing trap). */
  loopRound?: LoopRoundSpec
  /** M1/G3: per-agent model preference (e.g. "opus"/"sonnet"/"haiku"); the host resolves it to a
   *  provider via `RuntimeOptions.providerFor`. Host-side routing only — not sent to the kernel. */
  modelHint?: string
  /** M4/G5: cumulative token cap for this sub-agent's run (sets the child kernel's `maxTotalTokens`). */
  tokenBudget?: number
  /** O3: per-child turn cap (sets the child runner's `maxTurns`; falls back to the parent's). A child
   *  that exhausts it terminates `max_turns` — the parent reads the termination and decides retry/skip. */
  maxTurns?: number
  /** O3: per-child wall-clock cap in milliseconds (sets the child runner's `timeoutMs`; falls back to
   *  the parent's). A hung child terminates `timeout` instead of stalling the parent indefinitely. */
  maxWallMs?: number
  /** Tool surface for a spawned sub-agent. Host-side only (like `modelHint`) — NOT sent to the kernel
   *  (`agentRunSpecToKernel` maps fields explicitly and omits it). Default `"filtered"` keeps the spawn
   *  path's deny-all-safe default: the child is filtered to its manifest grants, and a grant-less spawn
   *  resolves to zero tools. `"inherit"` runs the child on the parent's execution plane with the
   *  parent's meta-tool availability (same mechanism trusted workflow nodes use) — the child's surface
   *  is a subset of the parent's, never a privilege escalation. */
  toolAccess?: "inherit" | "filtered"
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
  /** A#2 v2 loop stop signal: a loop iteration sets `false` to end the loop before `max_iters`.
   *  `undefined` (every non-loop result) ⇒ no opinion → run to the cap. Sent only when set. */
  loopContinue?: boolean
  /** A#2 classify routing: a classifier node reports the chosen branch label here; the kernel runs
   *  that branch and prunes the rest. Sent only when set. */
  classifyBranch?: string
  /** A#2 tournament verdict: a judge reports the winning entrant's agent id here. Sent only when set. */
  tournamentWinner?: string
  /** ③ loop-agent pacing: the kernel-adjudicated after-round decision, surfaced by the orchestrator
   *  from the child's done event. For a loop-node iteration this is the PRIMARY continuation
   *  vocabulary (stop → loopContinue=false); the legacy text-sniffed signal is the fallback.
   *  SDK-internal — stripped by `subAgentResultToKernel`. */
  paceDecision?: import("../runtime/kernel-step.js").PaceDecision
  /** Two-axis AttemptLoop result. Serialized alongside `termination` so hosts can observe judge
   *  failure independently from run health. */
  attempt?: {
    outcome: import("../harness/harness.js").AttemptOutcomeKind
    runStatus: string
    attempts: number
    verdict?: import("../runtime/eval.js").Verdict
  }
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
  if (spec.loopRound) {
    out.loop_round = {
      ...(spec.loopRound.maxRounds !== undefined ? { max_rounds: spec.loopRound.maxRounds } : {}),
      ...(spec.loopRound.minSleepMs !== undefined ? { min_sleep_ms: spec.loopRound.minSleepMs } : {}),
      ...(spec.loopRound.maxSleepMs !== undefined ? { max_sleep_ms: spec.loopRound.maxSleepMs } : {}),
      ...(spec.loopRound.defaultAction !== undefined ? { default_action: spec.loopRound.defaultAction } : {}),
    }
  }
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

/** Tool-call `arguments` reach us as a raw model-authored string (e.g. the OpenAIChat-family
 *  non-streaming path passes it through verbatim via `normalizeToolCalls`). A malformed JSON
 *  string must degrade to empty args here, never throw — otherwise one bad tool-call on a
 *  sub-agent's final turn bricks the parent's result serialization. Mirrors the `catch→{}`
 *  guard every provider/runtime parse site already uses. */
function safeParseToolArgs(raw: string | undefined): Record<string, unknown> {
  try {
    return JSON.parse(raw || "{}") as Record<string, unknown>
  } catch {
    return {}
  }
}

export function subAgentResultToKernel(result: SubAgentResult): Record<string, unknown> {
  const finalMessage = result.result.finalMessage
  const attempt = result.result.attempt
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
              arguments: safeParseToolArgs(tc.arguments),
            })),
            ...(finalMessage.tokenCount !== undefined ? { token_count: finalMessage.tokenCount } : {}),
          }
        : null,
      turns_used: result.result.turnsUsed,
      total_tokens_used: result.result.totalTokensUsed,
      // A#2: control-flow signals — additive, omitted on the wire when unset so a plain spawn's
      // result is byte-identical to before. The kernel reads each only for the matching node kind.
      ...(result.result.loopContinue !== undefined ? { loop_continue: result.result.loopContinue } : {}),
      ...(result.result.classifyBranch !== undefined ? { classify_branch: result.result.classifyBranch } : {}),
      ...(result.result.tournamentWinner !== undefined ? { tournament_winner: result.result.tournamentWinner } : {}),
      ...(attempt
        ? {
            attempt: {
              outcome: attempt.outcome,
              run_status: attempt.runStatus,
              attempts: attempt.attempts,
              ...(attempt.verdict
                ? {
                    verdict: {
                      passed: attempt.verdict.passed,
                      overall_score: attempt.verdict.overallScore,
                      feedback: attempt.verdict.feedback,
                      details: attempt.verdict.details.map(detail => ({
                        criterion: detail.criterion,
                        passed: detail.passed,
                        score: detail.score,
                        feedback: detail.feedback,
                      })),
                    },
                  }
                : {}),
            },
          }
        : {}),
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
export type WorkflowDependencyPolicy = "all_success" | "accept_partial" | "all_terminal" | "optional"
export type WorkflowNodeStatus = "completed" | "completed_partial" | "failed" | "skipped_upstream_failed"

export function workflowNodeStatusFromTermination(termination: TerminationReason | string): WorkflowNodeStatus {
  if (termination === "completed") return "completed"
  if (termination === "error" || termination === "user_abort") return "failed"
  return "completed_partial"
}

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
  /** A#2 v2: make this a *loop* node — re-run its agent up to `maxIters` times. An iteration may end
   *  the loop early by reporting `loopContinue: false` (the runner solicits this from the agent). */
  loop?: { maxIters: number }
  /** A#2: make this a *classify* node — its agent picks exactly one branch `label`; that branch's
   *  nodes run and the others are pruned. Each branch node must list this node's index in `dependsOn`. */
  classify?: { branches: Array<{ label: string; nodes: number[] }> }
  /** A#2: make this a *tournament controller* — generate each `entrants` candidate in parallel, then
   *  pairwise-judge them to one winner (this node's `task.goal` is the judging criterion). ≥2 entrants. */
  tournament?: { entrants: WorkflowTaskSpec[] }
  /** M4/G5: cap this node's child run at `tokenBudget` cumulative tokens (the per-node "use N tokens"). */
  tokenBudget?: number
  /** O3: cap this node's child run at `maxTurns` provider turns (falls back to the parent's). */
  maxTurns?: number
  /** O3: cap this node's child run at `maxWallMs` wall-clock milliseconds. */
  maxWallMs?: number
  /** Indices of nodes this node depends on. */
  dependsOn?: number[]
  /** How dependency terminal states gate this node. Defaults to `all_success`. */
  depPolicy?: WorkflowDependencyPolicy
}

/** A declarative workflow DAG the kernel runs node-by-node, gating each spawn. */
export interface WorkflowSpec {
  nodes: WorkflowNodeSpec[]
}

export interface KernelWorkflowNodeOutcome {
  node_id: string
  status: WorkflowNodeStatus
  termination?: TerminationReason
  output?: {
    role: Message["role"]
    content: string
    tool_calls?: Array<{ id: string; name: string; arguments?: Record<string, unknown> }>
    token_count?: number
  }
}

export interface WorkflowNodeOutcome {
  nodeId: string
  status: WorkflowNodeStatus
  termination?: TerminationReason
  output?: Message
}

/** A control-plane request rejected before any workflow effect started. */
export interface ControlRequestRejection {
  operation: string
  subject?: string
  reason: string
}

export interface WorkflowOutcome {
  nodeOutcomes: WorkflowNodeOutcome[]
  outputs: Record<string, string>
  /** Present when the workflow itself was rejected before any node ran. */
  rejection?: ControlRequestRejection
}

export function workflowNodeOutcomeFromKernel(raw: KernelWorkflowNodeOutcome): WorkflowNodeOutcome {
  const output = raw.output
  return {
    nodeId: raw.node_id,
    status: raw.status,
    ...(raw.termination ? { termination: raw.termination } : {}),
    ...(output
      ? {
          output: {
            role: output.role,
            content: output.content,
            toolCalls: (output.tool_calls ?? []).map(call => ({
              id: call.id,
              name: call.name,
              arguments: JSON.stringify(call.arguments ?? {}),
            })),
            ...(output.token_count != null ? { tokenCount: output.token_count } : {}),
          },
        }
      : {}),
  }
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
  /** The dependency agent ids for EVERY dependent node (W-N2: a DAG edge carries data). A reduce
   *  node's registered function consumes them; every other node gets its deps' outputs in context. */
  input_agent_ids?: string[]
  /** A#2: present only for a tournament *judge* spawn — the two entrant agent ids whose produced
   *  outputs this judge compares. The runner looks them up and reports the winner as `tournamentWinner`. */
  judge_match?: { left: string; right: string }
  /** A#2 v2: present only for a *loop* iteration spawn — the loop's `max_iters`. Marks the spawn as a
   *  loop iteration so the runner solicits + reports a `loopContinue` stop signal. */
  loop_max_iters?: number
  /** A#2: present only for a *classify* spawn — the branch labels the classifier must choose among.
   *  Non-empty marks the spawn as a classifier so the runner instructs the agent + reports `classifyBranch`. */
  classify_labels?: string[]
  /** M4/G5: the node's per-node cumulative token cap, if set — the runner caps the child run here. */
  token_budget?: number
  /** O3: per-node turn cap → the child run's `maxTurns`. */
  max_turns?: number
  /** O3: per-node wall-clock cap (ms) → the child run's timeout. */
  max_wall_ms?: number
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
  /** M4/G5 token headroom: cumulative tokens used, the run-level cap, and tokens remaining before the
   *  token budget terminates the run — so a coordinator can scale a submission to "use N tokens". */
  tokens_used?: number
  tokens_max?: number
  tokens_remaining?: number
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
  if (budget.tokens_remaining != null && budget.tokens_max != null) {
    parts.push(`tokens ${budget.tokens_used ?? 0}/${budget.tokens_max} used, ${budget.tokens_remaining} remaining`)
  }
  if (parts.length === 0) return ""
  return (
    `[workflow budget] ${parts.join(" · ")}. ` +
    "If you submit more workflow nodes, keep the batch within the remaining node and token budget."
  )
}

/** Normalize a `WorkflowTaskSpec` (object or bare goal string) to the kernel's `RuntimeTask` JSON. */
function workflowTaskToKernel(t: WorkflowTaskSpec): Record<string, unknown> {
  const task = typeof t === "string" ? { goal: t } : t
  return {
    goal: task.goal,
    // `criteria` is required by the kernel's RuntimeTask serde (no default).
    criteria: task.criteria ?? [],
    ...(task.lane ? { lane: task.lane } : {}),
  }
}

/** Lower a node's control-flow kind to the kernel's serde-tagged `NodeKind` JSON, or `undefined` for
 *  a plain spawn. `reducer` / `loop` / `classify` / `tournament` are mutually exclusive — declaring
 *  more than one is a spec error (a node has exactly one kind). */
function nodeKindToKernel(n: WorkflowNodeSpec): Record<string, unknown> | undefined {
  const declared = [n.reducer != null, n.loop != null, n.classify != null, n.tournament != null].filter(Boolean).length
  if (declared > 1) {
    throw new Error("a workflow node may declare at most one of: reducer, loop, classify, tournament")
  }
  if (n.reducer != null) return { type: "reduce", reducer: n.reducer }
  if (n.loop != null) return { type: "loop", max_iters: n.loop.maxIters }
  if (n.classify != null) {
    return { type: "classify", branches: n.classify.branches.map(b => ({ label: b.label, nodes: b.nodes })) }
  }
  if (n.tournament != null) return { type: "tournament", entrants: n.tournament.entrants.map(workflowTaskToKernel) }
  return undefined
}

/** Map one host `WorkflowNodeSpec` to its snake_case kernel JSON. Shared by `load_workflow` (the
 *  whole spec) and `submit_workflow_nodes` (R3-1 runtime append) so the two encodings never drift. */
export function workflowNodeSpecToKernel(n: WorkflowNodeSpec): Record<string, unknown> {
  const kind = nodeKindToKernel(n)
  return {
    task: workflowTaskToKernel(n.task),
    role: n.role,
    // role/isolation/context_inheritance have no serde default in the kernel — always emit.
    isolation: n.isolation ?? "shared",
    context_inheritance: n.contextInheritance ?? "none",
    ...(n.modelHint ? { model_hint: n.modelHint } : {}),
    ...(n.trust && n.trust !== "trusted" ? { trust: n.trust } : {}),
    ...(n.outputSchema ? { output_schema: n.outputSchema } : {}),
    // A#2/G2: loop / classify / tournament / reduce lower to a serde-tagged `NodeKind`; spawn omits it.
    ...(kind ? { kind } : {}),
    // M4/G5: per-node token cap (additive; omitted when unset).
    ...(n.tokenBudget != null ? { token_budget: n.tokenBudget } : {}),
    // O3: per-node turn / wall-clock caps (additive; omitted when unset).
    ...(n.maxTurns != null ? { max_turns: n.maxTurns } : {}),
    ...(n.maxWallMs != null ? { max_wall_ms: n.maxWallMs } : {}),
    ...(n.dependsOn && n.dependsOn.length ? { depends_on: n.dependsOn } : {}),
    dep_policy: n.depPolicy ?? "all_success",
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

/** M5/G1: map an agent-authored spec to the `submit_workflow` kernel event body (the agent-reachable
 *  `Syscall::LoadWorkflow`). The kernel bootstraps the DAG when none is active, else flattens onto it.
 *  `parentSessionId` seeds child session ids on bootstrap; `submitterAgentId` carries G1 trust coercion
 *  on the flatten case (a quarantined author's nodes are coerced quarantined). */
export function submitWorkflowToKernel(
  spec: WorkflowSpec,
  parentSessionId: string,
  submitterAgentId?: string,
): Record<string, unknown> {
  return {
    kind: "submit_workflow",
    spec: workflowSpecToKernel(spec),
    parent_session_id: parentSessionId,
    ...(submitterAgentId ? { submitter_agent_id: submitterAgentId } : {}),
  }
}

/** Shared JSON-Schema for a workflow-node batch (a DAG). Used by both `submit_workflow_nodes`
 *  (append) and `start_workflow` (M5 v1: author a sub-workflow), so the two tools never drift. */
const workflowNodesArraySchema = {
  type: "array",
  description:
    "Workflow nodes (a DAG); each runs as a gated sub-agent. A node may declare ONE control-flow kind " +
    "— `loop` / `classify` / `tournament` / `reducer` — otherwise it is a plain spawn. `dependsOn` and " +
    "`classify.branches[].nodes` are batch-relative (index 0 = this batch's first node).",
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
      modelHint: {
        type: "string",
        description: "Preferred model for this node (e.g. \"opus\"/\"sonnet\"/\"haiku\"); the host routes it.",
      },
      reducer: {
        type: "string",
        description: "Make this a deterministic reduce node (no LLM); names a registered reducer.",
      },
      loop: {
        type: "object",
        description: "Make this a loop node: re-run its agent up to maxIters times, ending early when it reports done.",
        properties: { maxIters: { type: "integer", description: "Hard iteration cap." } },
        required: ["maxIters"],
      },
      classify: {
        type: "object",
        description: "Make this a classify node: its agent picks one branch label; that branch's nodes run, the rest are pruned.",
        properties: {
          branches: {
            type: "array",
            items: {
              type: "object",
              properties: {
                label: { type: "string" },
                nodes: {
                  type: "array",
                  items: { type: "integer" },
                  description: "Batch-relative indices of the nodes to run when this branch is chosen.",
                },
              },
              required: ["label", "nodes"],
            },
          },
        },
        required: ["branches"],
      },
      tournament: {
        type: "object",
        description: "Make this a tournament controller: generate each entrant, then pairwise-judge to one winner (this node's task is the criterion).",
        properties: {
          entrants: {
            type: "array",
            description: "≥2 candidate tasks to generate and judge.",
            items: {
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
          },
        },
        required: ["entrants"],
      },
      tokenBudget: {
        type: "integer",
        description: "Cap this node's child run at this many cumulative tokens.",
      },
      dependsOn: {
        type: "array",
        items: { type: "integer" },
        description: "Batch-relative, backward-only dependency indices within this submission.",
      },
      depPolicy: {
        type: "string",
        enum: ["all_success", "accept_partial", "all_terminal", "optional"],
        description: "How dependency terminal states gate this node; defaults to all_success.",
      },
    },
    required: ["task", "role"],
  },
} as const

/** R3-1: the tool a workflow-coordinator node's agent calls to append work to the running DAG
 *  (true loop-until-done / dynamic fan-out). Give it to nodes meant to fan out; the runner intercepts
 *  the call and routes the nodes to the parent kernel (the child's own kernel holds no workflow). */
export const submitWorkflowNodesTool: ToolSchema = {
  name: "submit_workflow_nodes",
  description:
    "Append new nodes to the running workflow DAG (dynamic fan-out / loop-until-done). Each node " +
    "spawns as a gated sub-agent. Use when you discover more work that should run as its own node. " +
    "A node may declare ONE control-flow kind — `loop` (re-run until done), `classify` (route to one " +
    "branch), `tournament` (pairwise-judge candidates to a winner), or `reducer` (deterministic, no " +
    "LLM) — otherwise it is a plain spawn. Within a submission, `dependsOn` and `classify.branches[].nodes` " +
    "are batch-relative (index 0 = this batch's first node).",
  parameters: JSON.stringify({
    type: "object",
    properties: { nodes: workflowNodesArraySchema },
    required: ["nodes"],
  }),
}

/** M5 v1 (flatten): the tool an agent calls to **author a sub-workflow** — a cohesive DAG of nodes
 *  (incl. loop/classify/tournament/reduce) composed onto the running workflow. Mechanically it lowers
 *  to the same append path as `submit_workflow_nodes` (a `WorkflowSpec` is a node batch), but reads as
 *  "write a harness" rather than "append nodes". v2 adds top-level bootstrap (the `LoadWorkflow`
 *  kernel syscall) so a plain run can start a workflow from scratch. */
export const startWorkflowTool: ToolSchema = {
  name: "start_workflow",
  description:
    "Author and run a sub-workflow: a DAG of nodes (fan-out / classify / tournament / loop / reduce) " +
    "composed onto the current run. Use to structure a multi-step task as its own harness. The nodes " +
    "spawn as gated sub-agents; `dependsOn` / `classify.branches[].nodes` are spec-relative.",
  parameters: JSON.stringify({
    type: "object",
    properties: {
      spec: {
        type: "object",
        description: "The workflow specification.",
        properties: { nodes: workflowNodesArraySchema },
        required: ["nodes"],
      },
    },
    required: ["spec"],
  }),
}

/** Build a sub-agent run spec for a kernel-generated workflow node. */
export function workflowNodeToSpec(node: WorkflowSpawnInfo, parentSessionId: string): AgentRunSpec {
  // W-N6 transcript-as-carry: a loop node's iterations share ONE stable session id (the `-i{k}`
  // suffix names the spawn, not the session), so iteration k replays the transcript of 0..k-1 —
  // "do the next increment" actually sees the previous increments. The agent_id keeps the
  // per-iteration suffix (kernel completion routing).
  const sessionNodeId = node.loop_max_iters != null ? node.agent_id.replace(/-i\d+$/, "") : node.agent_id
  return {
    identity: {
      agentId: node.agent_id,
      sessionId: `${parentSessionId}-${sessionNodeId}`,
      isSubAgent: true,
      parentSessionId,
    },
    role: node.role as KernelAgentRole,
    isolation: node.isolation as AgentIsolation,
    goal: node.goal,
    // M1/G3: carry the node's model preference so the orchestrator can route to a provider.
    ...(node.model_hint ? { modelHint: node.model_hint } : {}),
    // M4/G5: carry the node's token cap so the orchestrator can bound the child run.
    ...(node.token_budget != null ? { tokenBudget: node.token_budget } : {}),
    // O3: carry the node's turn / wall-clock caps (the orchestrator already honors these).
    ...(node.max_turns != null ? { maxTurns: node.max_turns } : {}),
    ...(node.max_wall_ms != null ? { maxWallMs: node.max_wall_ms } : {}),
    // DW-3 one continuation vocabulary: a loop ITERATION runs with the pacing trap armed, so the
    // agent signals continue/stop through the kernel-adjudicated `pace` meta-tool instead of a
    // text-sniffed JSON blob. One iteration = one round; the DAG (not max_rounds) caps iterations,
    // and default stop means "ended without pacing" = done — the CC silence-is-completion contract.
    ...(node.loop_max_iters != null ? { loopRound: { defaultAction: "stop" as const } } : {}),
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
 * Generate→evaluate quality gate (the EvalPipeline successor, #6): a `loop` worker node (re-run up
 * to `maxIters`, stopping early on a `loop_continue=false` self-signal) + a bias-resistant `verify`
 * eval node gated on it, carrying the kernel's verdict `outputSchema`. Mirrors the kernel `gen_eval`
 * template. For the iterative retry-with-feedback variant, drive it with `AttemptLoop`.
 */
export function genEval(
  worker: WorkflowTaskSpec,
  evaluate: WorkflowTaskSpec,
  maxIters = 3,
  extractSkillOnPass = true,
): WorkflowSpec {
  const schema = JSON.parse(getKernel().verdictOutputSchema(extractSkillOnPass)) as Record<string, unknown>
  return {
    nodes: [
      {
        task: asTask(worker),
        role: "implement",
        isolation: "worktree",
        contextInheritance: "full",
        loop: { maxIters: Math.max(1, maxIters) },
      },
      {
        task: asTask(evaluate),
        role: "verify",
        isolation: "read_only",
        contextInheritance: "none",
        dependsOn: [0],
        outputSchema: schema,
      },
    ],
  }
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
