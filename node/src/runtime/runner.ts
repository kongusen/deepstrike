import type {
  LLMProvider, Message, ContentPart, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, WorkflowNodesSubmittedEvent, DoneEvent, ErrorEvent,
  ToolSuspendEvent, ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent,
  PermissionResponse, PermissionResolvedEvent, AsyncSummarizer, DreamSummarizer,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions,
} from "../types.js"
import type {
  DreamStore,
  MemoryRecord,
  MemoryRecall,
  MemoryScope,
  SessionData,
  MemoryQuery,
} from "../memory/protocols.js"
import { extractSessionMemories } from "../memory/extraction.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import type {
  RuntimeSignal,
  RuntimeSignalUrgency,
  SignalDeliveryReceipt,
  SignalSource,
} from "../signals/types.js"
import type { SessionLog, SessionEvent, RollbackReason } from "./session-log.js"
import type { ArchiveStore } from "./archive.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { GroupBudgetScope } from "./run-group.js"
import type { RunGroup, GroupBudgetRequest } from "./run-group.js"
import { getKernel, type KernelRuntimeInstance, type MemoryPolicy, type ResourceQuota } from "../kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  buildWorkflowNodeCompletedEvent,
  buildWorkflowNodesSubmittedEvent,
  recoverWorkflowNodeOutcomes,
  recoverSubmittedWorkflowNodes,
  repairEventsForRecovery,
  type RecoveredNodeOutcome,
} from "./session-repair.js"
import { KernelPrimitivesDashboard } from "./kernel-primitives-dashboard.js"
import {
  capabilityMarker,
  capabilitySkill,
  capabilityTool,
  capabilityCommandMount,
  capabilityCommandUnmount,
  entropySampleFromObservation,
  durableKernelAction,
  durableKernelApply,
  durableKernelMaybeAction,
  messageToKernelMessage,
  skillMetadataToKernel,
  taskUpdateToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
  type KernelObservation,
  type KernelRunnerAction,
} from "./kernel-step.js"
import type {
  AgentRunSpec, AgentProcessChangedObservation, MilestoneCheckResult, MilestoneContract, MilestonePolicy, SubAgentResult,
  WorkflowSpec, WorkflowSpawnInfo, WorkflowNodeSpec, WorkflowBudget, WorkflowOutcome,
  WorkflowNodeOutcome, KernelWorkflowNodeOutcome,
} from "../types/agent.js"
import {
  agentRunSpecToKernel,
  findSpawnProcessObservation,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  spawnObservationToManifest,
  subAgentResultToKernel,
  submitWorkflowNodesToKernel,
  submitWorkflowToKernel,
  workflowBudgetNote,
  workflowNodeOutcomeFromKernel,
  workflowNodeStatusFromTermination,
  workflowNodeToManifest,
  workflowNodeToSpec,
  workflowSpecToKernel,
} from "../types/agent.js"
import { defaultSubAgentOrchestrator, type SubAgentOrchestrator } from "./sub-agent-orchestrator.js"
import {
  extractJsonValue,
  schemaInstruction,
  schemaRetryInstruction,
  validateAgainstSchema,
} from "./output-schema.js"
import { resolveReducer, type ReducerRegistry } from "./reducers.js"
import {
  loopInstruction, classifyInstruction, judgeGoal, dependencyOutputsNote,
  extractLoopContinue, extractClassifyBranch, extractJudgeWinner,
} from "./workflow-control-flow.js"
import { governancePolicyToKernelEvent, governanceFilterSchema, type GovernancePolicy } from "../governance.js"
import { kernelObservationToSessionEvent } from "./kernel-event-log.js"
import { assertNativeProfile, type NativeOsProfile, type OsProfileId, type SignalPolicy } from "./os-profile.js"
import { LargeResultSpool } from "./large-result-spool.js"
import { formatToolError } from "../tools/errors.js"
import { ManagedTaskScope } from "./reliability.js"
import type { BackgroundTaskErrorHandler, OperationContext } from "./reliability.js"
import {
  contextPolicyV1,
  normalizeContextPolicyV1,
  type ContextPolicyOverridesV1,
} from "./context-policy.js"

export interface SchedulerPolicy {
  version: 1
  criticalPathWeight: number
  fanoutWeight: number
  ageWeight: number
  tokenCostWeight: number
}

export function schedulerPolicyToKernel(policy: SchedulerPolicy): Record<string, number> {
  const allowed = new Set([
    "version", "criticalPathWeight", "fanoutWeight", "ageWeight", "tokenCostWeight",
  ])
  const unknown = Object.keys(policy).filter(key => !allowed.has(key))
  if (unknown.length > 0) throw new TypeError(`unknown scheduler policy field(s): ${unknown.join(", ")}`)
  return {
    version: policy.version,
    critical_path_weight: policy.criticalPathWeight,
    fanout_weight: policy.fanoutWeight,
    age_weight: policy.ageWeight,
    token_cost_weight: policy.tokenCostWeight,
  }
}

/** Host-counted provider envelope and response reserves deducted from the model context window. */
export interface PromptBudget {
  promptOverheadTokens: number
  outputReserveTokens: number
  safetyMarginTokens: number
}

interface InboundSignalDelivery {
  signalId: string
  deliveryId: string
  deliveryAttempt: number
  signal: RuntimeSignal
  ack(): Promise<boolean>
  nack(): Promise<boolean>
}

/** P0-C tool-gating telemetry: per-LLM-turn metrics, emitted via `RuntimeOptions.onTurnMetrics`.
 *  Pure observation — no behavior change. Feeds the go/no-go analysis for epoch skill gating (P1-B):
 *  - `toolsExposed` vs `toolsCalled` quantifies over-exposure.
 *  - `activeSkill` across consecutive turns yields the skill *dwell* `D` (how long a skill stays
 *    loaded) — the break-even input that decides whether dynamic gating beats the cache-bust cost.
 *  - `cacheReadTokens` / `cacheCreationTokens` give the prompt-cache hit baseline to compare against
 *    after B/D ship. */
export interface TurnMetrics {
  /** 1-based kernel turn this LLM call belongs to. */
  turn: number
  /** Number of tool schemas exposed to the model this turn (base + meta, after run-profile gating). */
  toolsExposed: number
  /** Number of tool calls the model emitted this turn. */
  toolsCalled: number
  /** The skill loaded and in effect going into this turn (the most recent `skill` tool call's name),
   *  or undefined if none is active. Consecutive equal values measure dwell. */
  activeSkill?: string
  /** Full prompt size the provider reported (uncached + cache read + cache creation). */
  inputTokens: number
  /** Tokens served from the prompt cache this turn (Anthropic `cache_read_input_tokens`). */
  cacheReadTokens: number
  /** I1: per-slot attribution of `cacheReadTokens`. Anthropic reports a single cache-read total,
   *  not a per-block breakdown — this field is a pro-rata estimate over the slots that actually
   *  carried a `cache_control` breakpoint on the request. Missing / empty when the provider does
   *  not honor `cache_control` (OpenAI-family auto-cache) or when no breakpoints were placed.
   *  Useful for diagnosing which slot is buying the cache hit when comparing strategies. */
  cacheReadTokensBySlot?: { system?: number; tools?: number; messages?: number }
  /** Tokens written to the prompt cache this turn (Anthropic `cache_creation_input_tokens`). */
  cacheCreationTokens: number
}

/** O5: decision returned by `onToolCall` — `block: true` denies this call before it executes; the
 *  `reason` is fed back to the model as a governance-denied tool result (so it can redirect). */
export interface ToolCallHookDecision {
  block?: boolean
  reason?: string
}

/** O5: decision returned by `onToolResult` — `replaceOutput` swaps the result the model (and the
 *  session log) sees; `note` is injected into the signal stream (see `injectNote`). */
export interface ToolResultHookDecision {
  replaceOutput?: string
  note?: string
}

/** Bounded kernel reliability policy. Omitted fields retain kernel defaults. */
export interface KernelReliabilityOptions {
  /** Deduplicated input-event replay window, 1..65536. */
  eventReplayCapacity?: number
  /** Completed effect-result replay window, 1..65536. */
  completedEffectReplayCapacity?: number
  /** Provider overflow recovery retries, 0..16. */
  providerRecoveryAttempts?: number
  /** Truncated-output recovery retries, 0..16. */
  outputRecoveryAttempts?: number
  /** Host durability-effect retries, 0..16. */
  hostEffectRetryAttempts?: number
  /** Tool-result spool threshold in bytes; must be positive. */
  spoolThresholdBytes?: number
  /** Inline spool preview bytes; positive and no larger than the threshold. */
  spoolPreviewBytes?: number
  /** Max accepted ABI transactions retained for a portable KernelSnapshot rebuild. */
  snapshotInputLimit?: number
  /** Max canonical JSON bytes accepted for one kernel input, 256..64MiB. */
  maxInputBytes?: number
  /** Max canonical JSON bytes retained by the snapshot journal, 256..1GiB. */
  snapshotJournalBytesLimit?: number
}

export type OperationCancellationReason = "user" | "deadline" | "lease_lost" | "host_shutdown"

export interface RuntimeOptions {
  provider: LLMProvider
  /** M4/G5: cumulative token cap for this run (the kernel's `max_total_tokens`). A workflow node's
   *  `tokenBudget` flows here for its child run, so an expensive node self-terminates at the cap.
   *  Undefined ⇒ the kernel default. */
  maxTotalTokens?: number
  /** M1/G3 intelligence routing: resolve a per-node provider from a workflow node's `modelHint`
   *  (e.g. "opus" / "sonnet" / "haiku"). Returns undefined ⇒ fall back to `provider`. A workflow
   *  node carrying `model_hint` runs against the resolved provider; without this hook the hint is a
   *  no-op (the kernel still carries it for audit). */
  providerFor?: (modelHint: string) => LLMProvider | undefined
  /** M3/G4 worktree isolation: when set, an `isolation: "worktree"` sub-agent runs inside a git
   *  worktree this manager creates (and removes on completion), injected as `RunContext.cwd`.
   *  Undefined ⇒ worktree nodes fall back to the inherited plane (no isolation). */
  worktreeManager?: import("./worktree-plane.js").WorktreeManager
  sessionLog: SessionLog
  executionPlane: ExecutionPlane
  /** Receives failures from run-owned best-effort tasks after their semantic owner has committed. */
  onBackgroundTaskError?: BackgroundTaskErrorHandler
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  agentId?: string
  /** Required by host-generated memory queries and page-out writes. */
  memoryScope?: MemoryScope
  /** I4: optional run-start memory pre-fetch hook. The runner calls this ONCE per run, before the
   *  first LLM turn, with the request's goal and (optional) run-spec. Each returned scoped query
   *  becomes a `dreamStore.search(agentId, query)` and the resulting hits land in decaying
   *  HISTORY as an ordinary user turn before turn 1 (single-use retrieval content — never a
   *  permanent knowledge pin; `initialMemory` is the curated CLAUDE.md-analog seed). Returning
   *  `undefined` / empty array is a no-op. Requires `dreamStore` + `agentId`; missing either ⇒
   *  silently skipped (errs-open). Default when unset: one query = the run goal (P10). Bench memory-recall shows -57% turns / -55% dollars when
   *  relevant memories land on turn 1 instead of being discovered via the meta-tool on turn 3+. */
  preQueryMemory?: (ctx: {
    goal: string
    runSpec?: AgentRunSpec
    /** K4: `"initial"` = the once-per-run pre-turn-1 fetch; `"renewal"` = re-fired after a sprint
     *  renewal (renewal drops the old history INCLUDING earlier memory hits, so the new sprint
     *  gets a fresh recall pass). Hooks that ignore it keep the pre-K4 behavior. */
    phase?: "initial" | "renewal"
  }) => Promise<MemoryQuery[] | undefined> | MemoryQuery[] | undefined
  systemPrompt?: string
  initialMemory?: string[]
  skillDir?: string
  dreamStore?: DreamStore
  /** M4: advisory callback when a recalled record crosses the promotion threshold. The host/model
   *  decides whether to pin the record or promote its content into knowledge. */
  onPromotionSuggested?: (info: { recordId: string; recallCount: number }) => void
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  /** Named or concrete OS profile. Defaults to the native microkernel profile. */
  osProfile?: OsProfileId | NativeOsProfile
  /**
   * Declarative governance policy loaded into the kernel (`load_governance_policy`).
   * The kernel enforces deny/veto/rate-limit/param-constraint before tools execute;
   * AskUser calls surface as `tool_gated` and run through `onPermissionRequest`.
   */
  governancePolicy?: GovernancePolicy
  /** Version-1 in-kernel signal admission, queue, and expiry policy. */
  signalPolicy?: SignalPolicy
  /** Provider-envelope overhead plus output and safety reserves journaled before start. */
  promptBudget?: PromptBudget
  /** Stable replayable context behavior; SDK ratios are normalized to integer ppm on the ABI wire. */
  contextPolicy?: ContextPolicyOverridesV1
  /** Versioned deterministic DAG scheduling policy installed atomically through ConfigureRun. */
  schedulerPolicy?: SchedulerPolicy
  /**
   * Optional declarative resource quotas (`set_resource_quota`). Bounds spawn concurrency /
   * nesting depth and memory-write rate at the kernel's single syscall trap. When unset, spawn
   * and memory-write syscalls are admitted unconditionally (pre-M2 behavior).
   */
  resourceQuota?: ResourceQuota
  /** Host-selectable bounded replay/recovery/durability policy. */
  kernelReliability?: KernelReliabilityOptions
  /** Attempts allowed for a workflow node to satisfy its output schema, 1..16. Default: 2. */
  workflowSchemaValidationAttempts?: number
  /**
   * O6: the in-kernel repeat fuse — the hard rungs above the soft no-progress STOP. When the model
   * re-issues the IDENTICAL tool call (same name AND args) `denyAfter` turns in a row, the kernel
   * denies it and feeds a directive note back; at `terminateAfter` the run ends `no_progress`.
   * Same-tool/different-args loops never trip it. Defaults: enabled, denyAfter 5, terminateAfter 8.
   * Pass `false` to disable (e.g. legit fixed-argument polling loops).
   */
  repeatFuse?: { denyAfter?: number; terminateAfter?: number } | false
  /**
   * O4: the turn-end criteria gate (the Stop-hook analog). When the model tries to finish while the
   * run's `criteria` stand, the kernel injects ONE self-check turn ("verify each criterion; continue
   * if any is unmet") before accepting completion. Fires at most once per run; runs without criteria
   * are untouched. Default enabled — set `false` to accept the first finish unconditionally.
   */
  criteriaGate?: boolean
  /**
   * K2: max share of `maxTokens` the durable knowledge partition may occupy. Exceeding it emits a
   * `knowledge_budget_exceeded` observation (once per cache generation) and evicts the OLDEST
   * unpinned, non-skill entries at the next compaction/renewal boundary until usage fits. Pinned
   * entries and skill pins are never budget-evicted. `0` disables. Default: kernel's 0.25.
   */
  knowledgeBudgetRatio?: number
  /**
   * Opt-in kernel entropy watch: threshold alerting over the per-turn session-entropy score
   * (`entropy_sample` events stream unconditionally regardless). When the score crosses
   * `threshold` — armed via hysteresis and past the cooldown — the run emits an `entropy_alert`
   * stream event (and session-log record); with `notifyModel` the kernel also feeds the model a
   * durable `[SIGNAL]` directive. Absent ⇒ disabled (kernel default).
   */
  entropyWatch?: EntropyWatchOptions
  /**
   * K3: default lease (in turns) for every skill activation. After that many turns the kernel
   * auto-deactivates the skill — toolset re-widens, knowledge pin boundary-swept — exactly like
   * an explicit `deactivateSkill()`. Absent ⇒ activations are permanent (default). A repeat
   * `skill(name)` call refreshes the lease.
   */
  skillLeaseTurns?: number
  /**
   * O5 (the PreToolUse-hook analog): called for each kernel-APPROVED tool call just before it
   * executes. Return `{ block: true, reason }` to veto — the call never runs and the reason is fed
   * back to the model as a denied tool result. This is the seam for STATEFUL host policy (count
   * repeats, budget writes per resource, project-specific rules); keep static allow/deny in
   * `governancePolicy`. A throwing decision hook fails closed by default.
   */
  onToolCall?: (call: { callId: string; name: string; arguments: string }) =>
    Promise<ToolCallHookDecision | undefined | void> | ToolCallHookDecision | undefined | void
  /** Failure policy for `onToolCall`. Default `closed`; set `open` only for advisory hooks. */
  onToolCallFailure?: "closed" | "open"
  /**
   * O5 (the PostToolUse-hook analog): called for each executed tool result before it reaches the
   * kernel. Return `{ replaceOutput }` to swap the result the model sees (redact / annotate), and/or
   * `{ note }` to push a contextual note into the signal stream (same channel as `injectNote` —
   * e.g. "that write was a no-op, stop repeating it"). Errs-open: a throwing hook changes nothing.
   */
  onToolResult?: (result: { callId: string; name: string; arguments: string; output: string; isError: boolean }) =>
    Promise<ToolResultHookDecision | undefined | void> | ToolResultHookDecision | undefined | void
  /**
   * L1 (RunGroup): bind this runner to a governance domain shared by N peer sessions of one logical
   * run. Members pass the same `id` + `budgetStore`; reservable stores enforce the kernel's run-level
   * cap against settled + in-flight usage. Stores must implement atomic reserve/settle/release.
   * Unset ⇒ N=1. Only cumulative budget is shared; instantaneous
   * concurrency stays vehicle-scoped (spec §2.5).
   */
  runGroup?: RunGroup
  /**
   * Optional long-term memory policy (`set_memory_policy`). Tunes the kernel's memory subsystem
   * (retrieval top-k, stale-warning age, write validation, memory path). Unset leaves the kernel
   * defaults. Enabling memory still requires `dreamStore` + `agentId`.
   */
  memoryPolicy?: MemoryPolicy
  tokenizer?: string
  enablePlanTool?: boolean
  /**
   * Persist full tool outputs when the kernel emits `large_result_spooled`.
   * Defaults to `.spool/` under the process cwd.
   */
  resultSpool?: LargeResultSpool
  compressionStore?: ArchiveStore
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  /** Default: terminate — stop with milestone_pending when a phase needs evaluation. */
  milestonePolicy?: MilestonePolicy
  /** Optional external verifier when milestonePolicy is not auto_pass. */
  onMilestoneEvaluate?: (ctx: {
    phaseId: string
    criteria: string[]
    requiredEvidence: string[]
  }) => Promise<MilestoneCheckResult> | MilestoneCheckResult
  /** Passed to kernel start_run for role/isolation metadata. */
  runSpec?: AgentRunSpec
  /** P0-A tool gating: a static per-run tool profile — only these tool ids (plus the
   *  skill/memory/knowledge/update_plan meta-tools) are exposed to the model each turn.
   *  Sugar that lowers to the same `capability_filter` sub-agents use; byte-stable across
   *  the run, so it never busts the prompt-cache prefix. Augments `runSpec`'s filter when
   *  both are set; synthesizes a minimal run spec when `runSpec` is absent. Omitted/empty
   *  ⇒ all registered tools exposed (no gating). */
  allowedToolIds?: string[]
  /** P0-C: optional per-turn metrics sink for tool-gating telemetry (see `TurnMetrics`). Pure
   *  observation; invoked once per LLM turn. Never throws into the run loop (errors are swallowed). */
  onTurnMetrics?: (metrics: TurnMetrics) => void
  /** P1-B/D stable-core: tool ids that stay exposed even when an active skill narrows the toolset
   *  (read/search/bash etc.). Empty/absent ⇒ skills narrow to exactly their declared `allowed_tools`
   *  + meta-tools. Opt-in: with no skill declaring `allowed_tools`, gating never engages. */
  stableCoreToolIds?: string[]
  /** Loaded via load_milestone_contract before run start. */
  milestoneContract?: MilestoneContract
  /** Custom sub-agent host driver; defaults to SubAgentOrchestrator. */
  subAgentOrchestrator?: SubAgentOrchestrator
  /** M5 v2.1: marks this runner as executing AS a workflow node (a child spawned by the workflow
   *  driver). A workflow node's `start_workflow` FLATTENS to the parent kernel (emits
   *  `workflow_nodes_submitted` for `runWorkflow` to append). A top-level run (this flag unset)
   *  instead AUTO-PIVOTS: it bootstraps + drives the authored workflow in its own kernel and resumes
   *  the reason loop with the outcome. The orchestrator sets this on workflow-node children so a
   *  nested `start_workflow` flattens rather than recursing. */
  isWorkflowNode?: boolean
  /**
   * When set, sub-agents run through AttemptLoop with a RuntimeAttemptBody and LLM judge.
   * The eval provider evaluates the sub-agent's output against the criteria
   * from the AgentRunSpec, retrying up to maxAttempts times.
   */
  subAgentHarness?: {
    evalProvider: LLMProvider
    maxAttempts?: number
  }
  /** G2: custom reducers for `NodeKind::Reduce` workflow nodes, merged over the built-ins
   *  (`concat` / `dedupe_lines` / `merge_json_arrays` / `count`). A reduce node runs no LLM. */
  reducers?: ReducerRegistry
  /** Optional system prompt injected into the dream synthesis call. */
  dreamSystemPrompt?: string
  /** Custom LLM provider used for background memory consolidation (dream loop). */
  dreamProvider?: LLMProvider
  /**
   * Optional LLM summarizer for semantic page_out events. When unset, `dreamProvider`
   * (or the runtime provider) is used to produce long-term summaries for DreamStore.
   */
  dreamSummarizer?: DreamSummarizer
  /**
   * Optional async LLM summarizer. When provided, a background call is fired
   * after each compression event to produce a richer semantic summary.
   * The result is written back to SessionLog as `summary_upgraded` and used
   * on the next wake() in place of the rule-based summary.
   */
  asyncSummarizer?: AsyncSummarizer
  /** Enable real-time CLI diagnostics dashboard grouped by the three kernel primitives (Syscall, Sched, Mm) */
  enableDiagnosticsDashboard?: boolean
}

/** Kernel observation kinds owned by the memory lifecycle consumer (journal + store mirror). */
function isMemoryLifecycleObservation(obs: KernelObservation): boolean {
  return obs.kind === "memory_written"
    || obs.kind === "memory_queried"
    || obs.kind === "memory_validation_failed"
    || obs.kind === "memory_recalled"
    || obs.kind === "promotion_suggested"
}

function controlRequestRejection(
  observations: KernelObservation[],
  operation?: string,
): { operation: string; subject?: string; reason: string } | undefined {
  const rejected = observations.find(observation =>
    observation.kind === "control_request_rejected"
      && (!operation || observation.operation === operation),
  )
  if (!rejected) return undefined
  return {
    operation: rejected.operation ?? operation ?? "control_request",
    ...(rejected.subject ? { subject: rejected.subject } : {}),
    reason: typeof rejected.reason === "string" ? rejected.reason : "request denied",
  }
}

function pendingCallIds(action: KernelRunnerAction): string[] {
  switch (action.kind) {
    case "call_provider":
      return [action.effectId]
    case "execute_tool":
      return action.calls.map(call => call.id)
    case "request_approval":
      return action.requests.map(request => request.callId)
    case "spawn_workflow":
      return action.nodes.map(node => String(node.agent_id ?? "")).filter(Boolean)
    case "preempt_sub_agents":
      return action.agentIds
    default:
      return "effectId" in action ? [action.effectId] : []
  }
}

export class RuntimeRunner {
  private interrupted = false
  private cancellationReason: OperationCancellationReason | undefined
  /** Aborts host-owned provider I/O before `cancel_operation` commits the kernel terminal fact. */
  private abortController: AbortController | null = null
  private activeKernel: KernelRuntimeInstance | null = null
  private activeGroupBudgetScope: GroupBudgetScope | undefined
  private pendingObservations: KernelObservation[] = []
  private currentSessionId: string | null = null
  /** O2 (system-reminder channel): host-pushed notes awaiting the next turn-boundary drain. */
  private injectedSignals: RuntimeSignal[] = []
  /** Skill names whose content has already been pushed into the durable `knowledge` slot this
   *  run — guards against re-pushing a duplicate entry if the model calls `skill(name)` again for
   *  an already-active skill (loading is idempotent; the knowledge push should be too). */
  private knowledgePushedSkills = new Set<string>()
  private nextArchiveStart = 0
  private pendingPageOutArchives: Array<{ archiveStart: number; compressedSeq: number }> = []
  private activePageOutArchive: { archiveStart: number; compressedSeq: number } | undefined
  /** K4: the active run's goal, kept for the renewal-boundary memory re-query. */
  private currentGoal = ""
  /** M5 v2.1: sub-workflow specs a top-level agent authored via `start_workflow`, awaiting auto-drive
   *  at the next safe point (after the tool turn resolves, kernel back in Reason — not suspended). */
  private pendingAuthoredWorkflows: WorkflowSpec[] = []
  private workflowContinuation: Extract<KernelRunnerAction, { kind: "call_provider" }> | null = null
  private dashboard: KernelPrimitivesDashboard | null = null
  /** Most recent kernel entropy sample of the active/last run (see `latestEntropy`). */
  private lastEntropySample: EntropySample | null = null

  constructor(private readonly opts: RuntimeOptions) {
    const schemaAttempts = opts.workflowSchemaValidationAttempts ?? 2
    if (!Number.isInteger(schemaAttempts) || schemaAttempts < 1 || schemaAttempts > 16) {
      throw new RangeError("workflowSchemaValidationAttempts must be an integer between 1 and 16")
    }
    if (opts.enableDiagnosticsDashboard) {
      const originalAppend = opts.sessionLog.append.bind(opts.sessionLog)
      opts.sessionLog.append = async (sessionId, event) => {
        const seq = await originalAppend(sessionId, event)
        if (this.dashboard) {
          this.dashboard.ingest(event)
          this.dashboard.print()
        }
        return seq
      }
    }
  }

  /** Host configuration (for coordinator / sub-agent spawn). */
  get hostOptions(): RuntimeOptions {
    return this.opts
  }

  private durableSessionId(sessionId?: string | null): string {
    const resolved = sessionId ?? this.currentSessionId
    if (!resolved) throw new Error("durable kernel transitions require a session id")
    return resolved
  }

  private async commitKernelApply(
    runtime: KernelRuntimeInstance,
    pending: KernelObservation[],
    event: Record<string, unknown>,
    sessionId?: string | null,
  ): Promise<KernelObservation[]> {
    return durableKernelApply(
      runtime,
      this.opts.sessionLog,
      this.durableSessionId(sessionId),
      pending,
      event,
    )
  }

  private async commitKernelMaybeAction(
    runtime: KernelRuntimeInstance,
    pending: KernelObservation[],
    event: Record<string, unknown>,
    sessionId?: string | null,
  ): Promise<KernelRunnerAction | null> {
    return durableKernelMaybeAction(
      runtime,
      this.opts.sessionLog,
      this.durableSessionId(sessionId),
      pending,
      event,
    )
  }

  private async commitKernelAction(
    runtime: KernelRuntimeInstance,
    pending: KernelObservation[],
    event: Record<string, unknown>,
    sessionId?: string | null,
  ): Promise<KernelRunnerAction> {
    return durableKernelAction(
      runtime,
      this.opts.sessionLog,
      this.durableSessionId(sessionId),
      pending,
      event,
    )
  }

  private async persistMemoryToStore(memory: MemoryRecord, agentId: string): Promise<void> {
    if (!this.opts.dreamStore) throw new Error("memory persistence requires dreamStore")
    await this.opts.dreamStore.upsert(agentId, memory)
  }

  private async retrieveMemoryFromStore(
    query: MemoryQuery,
    requestedK: number,
    agentId: string,
  ): Promise<MemoryRecall[]> {
    if (!this.opts.dreamStore) throw new Error("memory queries require dreamStore")
    return (await this.opts.dreamStore.search(agentId, { ...query, top_k: requestedK }))
      .slice(0, requestedK)
  }

  /**
   * T5: run one memory query through the kernel's `query_memory → memory_query_result`
   * effect lifecycle on the given runtime. The kernel injects each routed hit into history
   * itself and derives the recall lifecycle (`memory_recalled`, edge-triggered
   * `promotion_suggested`) from the routed hits — the store stays a pure query.
   *
   * `seenRecordIds` is one dedupe horizon: a record hit by several queries of the same
   * prefetch is routed (recalled, injected) once. The kernel derives counts statelessly from
   * each hit's payload, so host-side pre-filtering is the only place duplicates can be stopped.
   *
   * The recall lifecycle is consumed immediately rather than via the per-turn drain: the store
   * must be up to date before any same-turn re-query, and a renewal prefetch fires inside the
   * drain loop where queued observations would not be consumed until the next boundary.
   * Non-memory observations are forwarded to `leftovers` (the run's pending queue) when given,
   * and discarded for detached syscall runtimes (their kernel is throwaway).
   */
  private async queryMemoryThroughKernel(
    runtime: KernelRuntimeInstance,
    query: MemoryQuery,
    agentId: string,
    sessionId: string | null | undefined,
    seenRecordIds?: Set<string>,
    leftovers?: KernelObservation[],
  ): Promise<{ hits: MemoryRecall[]; action: KernelRunnerAction | null }> {
    const observations: KernelObservation[] = []
    const action = await this.commitKernelAction(
      runtime,
      observations,
      { kind: "query_memory", query },
      sessionId,
    )
    if (action.kind !== "query_memory") {
      throw new Error(`query_memory returned unexpected kernel effect: ${action.kind}`)
    }

    let hits: MemoryRecall[] = []
    let ioError: unknown
    try {
      hits = await this.retrieveMemoryFromStore(query, action.requestedK, agentId)
      if (seenRecordIds) {
        hits = hits.filter(hit => {
          const id = hit.record.record_id
          if (seenRecordIds.has(id)) return false
          seenRecordIds.add(id)
          return true
        })
      }
    } catch (cause) {
      ioError = cause
    }
    // Close the kernel effect even when the store failed — never leave a dangling query.
    // The result commit resumes the kernel's reasoning path (`resume_after_preload`), which
    // re-emits the next loop action with the routed hits now in history — callers on a live
    // run kernel must adopt it (a query is a preload, not a detached side-channel).
    const resumed = await this.commitKernelMaybeAction(runtime, observations, {
      kind: "memory_query_result",
      effect_id: action.effectId,
      hits,
      ...(ioError ? { error: formatToolError(ioError) } : {}),
    }, sessionId)
    await this.consumeMemoryLifecycleObservations(sessionId, observations)
    if (leftovers) {
      for (const obs of observations) {
        if (!isMemoryLifecycleObservation(obs)) leftovers.push(obs)
      }
    }
    if (ioError) throw ioError
    return { hits, action: resumed }
  }

  async writeMemory(
    memory: MemoryRecord,
    opts: { sessionId?: string; agentId?: string } = {},
  ): Promise<void> {
    const sessionId = opts.sessionId ?? this.currentSessionId
    const agentId = opts.agentId ?? this.opts.agentId
    if (!this.opts.dreamStore || !agentId) return
    const durableSessionId = this.durableSessionId(sessionId)

    const observations: KernelObservation[] = []
    const runtime = this.createSyscallRuntime()
    const action = await this.commitKernelMaybeAction(
      runtime,
      observations,
      { kind: "write_memory", memory },
      durableSessionId,
    )
    if (!action) {
      await this.consumeMemoryLifecycleObservations(sessionId, observations)
      return
    }
    if (action.kind !== "persist_memory") {
      throw new Error(`write_memory returned unexpected kernel effect: ${action.kind}`)
    }

    let ioError: unknown
    try {
      await this.persistMemoryToStore(action.memory as unknown as MemoryRecord, agentId)
    } catch (cause) {
      ioError = cause
    }
    await this.commitKernelApply(runtime, observations, {
      kind: "memory_persist_result",
      effect_id: action.effectId,
      ...(ioError ? { error: formatToolError(ioError) } : {}),
    }, durableSessionId)
    await this.consumeMemoryLifecycleObservations(durableSessionId, observations)
    if (ioError) throw ioError
  }

  async queryMemory(
    query: MemoryQuery,
    opts: { sessionId?: string; agentId?: string } = {},
  ): Promise<MemoryRecall[]> {
    const sessionId = opts.sessionId ?? this.currentSessionId
    const agentId = opts.agentId ?? this.opts.agentId
    if (!this.opts.dreamStore || !agentId) return []
    const durableSessionId = this.durableSessionId(sessionId)

    // Detached syscall runtime: the kernel's history injection and resumed action are inert
    // (the kernel is throwaway), but the recall lifecycle is identical to an in-run query
    // (T5 parity).
    const runtime = this.createSyscallRuntime()
    const { hits } = await this.queryMemoryThroughKernel(runtime, query, agentId, durableSessionId)
    await this.logMemoryRetrievalResult(durableSessionId, hits)
    return hits
  }

  private async logMemoryRetrievalResult(
    sessionId: string | null | undefined,
    hits: MemoryRecall[],
  ): Promise<void> {
    if (!sessionId) return
    // The session-log record is the durable audit artifact; the kernel needs no
    // acknowledgment (the former kernel event was a no-op and was removed).
    await this.opts.sessionLog.append(sessionId, {
      kind: "memory_retrieval_result",
      hits,
    })
  }

  private createSyscallRuntime(): KernelRuntimeInstance {
    const { KernelRuntime } = getKernel()
    return new KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: this.opts.maxTurns,
      // M4/G5: per-node token cap → child run's cumulative token budget.
      maxTotalTokens: this.opts.maxTotalTokens !== undefined ? BigInt(this.opts.maxTotalTokens) : undefined,
      timeoutMs: this.opts.timeoutMs !== undefined ? BigInt(this.opts.timeoutMs) : undefined,
    })
  }

  private groupBudgetRequest(includeTokens = true): GroupBudgetRequest {
    const tokens = includeTokens ? this.opts.maxTotalTokens : undefined
    const subagents = this.opts.resourceQuota?.maxTotalSubagents
    const rounds = this.opts.runSpec?.loopRound ? 1 : undefined
    const roundLimit = this.opts.runSpec?.loopRound?.maxRounds
    return {
      limits: {
        ...(tokens !== undefined ? { tokens } : {}),
        ...(subagents !== undefined ? { subagents } : {}),
        ...(roundLimit !== undefined ? { rounds: roundLimit } : {}),
      },
      requested: {
        ...(tokens !== undefined ? { tokens } : {}),
        ...(subagents !== undefined ? { subagents } : {}),
        ...(rounds !== undefined ? { rounds } : {}),
      },
    }
  }

  private async settleGroupBudget(
    scope: GroupBudgetScope,
    actual: { tokens?: number; subagents?: number; rounds?: number },
  ): Promise<void> {
    const retries = this.opts.kernelReliability?.hostEffectRetryAttempts ?? 3
    for (let attempt = 0; ; attempt += 1) {
      try {
        await scope.settle(actual)
        return
      } catch (error) {
        if (attempt >= retries) throw error
      }
    }
  }

  /**
   * Lower the declarative governance / attention / scheduler-budget / resource-quota policies into a
   * freshly-created kernel. Shared by `execute()` (full agent run) and `bootstrapWorkflowKernel()`
   * (standalone host-driven workflow) so a workflow's DAG-node spawns are gated, queued, and quota'd
   * exactly as a mid-run spawn would be. Must run BEFORE `start_run` so the in-kernel gate enforces
   * every policy from the first spawn. No config ⇒ the native-profile defaults (铁律: defaults only).
   */
  private async applyKernelPolicies(
    runtime: KernelRuntimeInstance,
    groupBudgetScope?: GroupBudgetScope,
  ): Promise<void> {
    // K2: lower governance / attention / scheduler / quota in ONE `configure_run` event instead of
    // the previous 2–4 separate `set_*` / `load_governance_policy` events. The kernel applies each
    // present field via the same path its granular event uses; absent fields are left untouched.
    // (Requires the 0.2.30 core that ships `configure_run`.)
    const osProfile = assertNativeProfile(this.opts.osProfile ?? "native")
    const signalPolicy = this.opts.signalPolicy ?? osProfile.signalPolicy
    const governancePolicy = this.opts.governancePolicy ?? osProfile.governancePolicy

    // Strip the event `kind` off the governance event — `configure_run.config.governance` carries the
    // bare policy fields (default_action / rules / vetoed_tools / rate_limits / constraints).
    const { kind: _govKind, ...governance } = governancePolicyToKernelEvent(governancePolicy) as Record<string, unknown>

    const config: Record<string, unknown> = { governance }
    if (this.opts.contextPolicy) {
      config.context_policy = normalizeContextPolicyV1(contextPolicyV1(this.opts.contextPolicy))
    }
    if (this.opts.kernelReliability) {
      const reliability = this.opts.kernelReliability
      config.reliability = {
        ...(reliability.eventReplayCapacity !== undefined
          ? { event_replay_capacity: reliability.eventReplayCapacity }
          : {}),
        ...(reliability.completedEffectReplayCapacity !== undefined
          ? { completed_effect_replay_capacity: reliability.completedEffectReplayCapacity }
          : {}),
        ...(reliability.providerRecoveryAttempts !== undefined
          ? { provider_recovery_attempts: reliability.providerRecoveryAttempts }
          : {}),
        ...(reliability.outputRecoveryAttempts !== undefined
          ? { output_recovery_attempts: reliability.outputRecoveryAttempts }
          : {}),
        ...(reliability.hostEffectRetryAttempts !== undefined
          ? { host_effect_retry_attempts: reliability.hostEffectRetryAttempts }
          : {}),
        ...(reliability.spoolThresholdBytes !== undefined
          ? { spool_threshold_bytes: reliability.spoolThresholdBytes }
          : {}),
        ...(reliability.spoolPreviewBytes !== undefined
          ? { spool_preview_bytes: reliability.spoolPreviewBytes }
          : {}),
        ...(reliability.snapshotInputLimit !== undefined
          ? { snapshot_input_limit: reliability.snapshotInputLimit }
          : {}),
        ...(reliability.maxInputBytes !== undefined
          ? { max_input_bytes: reliability.maxInputBytes }
          : {}),
        ...(reliability.snapshotJournalBytesLimit !== undefined
          ? { snapshot_journal_bytes_limit: reliability.snapshotJournalBytesLimit }
          : {}),
      }
    }
    config.signal_policy = {
      version: 1,
      queue_max: signalPolicy.queueMax,
      ...(signalPolicy.ttlMs !== undefined ? { ttl_ms: signalPolicy.ttlMs } : {}),
      ...(signalPolicy.deadlineEscalation !== undefined
        ? { deadline_escalation: signalPolicy.deadlineEscalation }
        : {}),
    }
    if (this.opts.promptBudget) {
      config.prompt_budget = {
        prompt_overhead_tokens: this.opts.promptBudget.promptOverheadTokens,
        output_reserve_tokens: this.opts.promptBudget.outputReserveTokens,
        safety_margin_tokens: this.opts.promptBudget.safetyMarginTokens,
      }
    }
    if (this.opts.schedulerPolicy) {
      config.scheduler_policy = schedulerPolicyToKernel(this.opts.schedulerPolicy)
    }
    if (this.opts.resourceQuota) {
      const q = this.opts.resourceQuota
      config.resource_quota = {
        ...(q.maxConcurrentSubagents !== undefined ? { max_concurrent_subagents: q.maxConcurrentSubagents } : {}),
        ...(q.maxTotalSubagents !== undefined ? { max_total_subagents: q.maxTotalSubagents } : {}),
        ...(q.maxSpawnDepth !== undefined ? { max_spawn_depth: q.maxSpawnDepth } : {}),
        ...(q.maxWorkflowNodes !== undefined ? { max_workflow_nodes: q.maxWorkflowNodes } : {}),
        ...(q.memoryWritesPerWindow !== undefined
          ? { memory_writes_per_window: [q.memoryWritesPerWindow.maxWrites, q.memoryWritesPerWindow.windowMs] }
          : {}),
      }
    }

    if (groupBudgetScope) {
      config.budget_grant = {
        reservation_id: groupBudgetScope.reservationId,
        ...(groupBudgetScope.granted.tokens !== undefined
          ? { tokens: groupBudgetScope.granted.tokens }
          : {}),
        ...(groupBudgetScope.granted.subagents !== undefined
          ? { subagents: groupBudgetScope.granted.subagents }
          : {}),
        ...(groupBudgetScope.granted.rounds !== undefined
          ? { rounds: groupBudgetScope.granted.rounds }
          : {}),
      }
    }
    // O6: tune/disable the in-kernel repeat fuse. `false` disables; an object overrides thresholds.
    // Absent ⇒ kernel defaults (enabled, deny_after=5, terminate_after=8).
    if (this.opts.repeatFuse !== undefined) {
      const rf = this.opts.repeatFuse
      config.repeat_fuse = rf === false
        ? { enabled: false, deny_after: 0, terminate_after: 0 }
        : { enabled: true, deny_after: rf.denyAfter ?? 5, terminate_after: rf.terminateAfter ?? 8 }
    }
    // O4: turn-end criteria gate toggle (absent ⇒ kernel default: enabled).
    if (this.opts.criteriaGate !== undefined) {
      config.criteria_gate = this.opts.criteriaGate
    }
    // K2: knowledge budget ratio (absent ⇒ kernel default 0.25; 0 disables).
    if (this.opts.knowledgeBudgetRatio !== undefined) {
      config.knowledge_budget_ratio = this.opts.knowledgeBudgetRatio
    }
    // Entropy watch (opt-in): threshold alerting over the per-turn session-entropy score.
    // Absent fields keep kernel defaults (threshold 0.65 / hysteresis 0.1 / cooldown 4).
    if (this.opts.entropyWatch !== undefined) {
      const ew = this.opts.entropyWatch
      config.entropy_watch = {
        enabled: ew.enabled ?? true,
        ...(ew.threshold !== undefined ? { threshold: ew.threshold } : {}),
        ...(ew.hysteresis !== undefined ? { hysteresis: ew.hysteresis } : {}),
        ...(ew.cooldownTurns !== undefined ? { cooldown_turns: ew.cooldownTurns } : {}),
        ...(ew.notifyModel !== undefined ? { notify_model: ew.notifyModel } : {}),
      }
    }

    await this.commitKernelApply(runtime, this.pendingObservations, { kind: "configure_run", config })
  }

  /**
   * Mirror one kernel memory-lifecycle observation into the durable store / host callbacks.
   * Shared by the main run drain, the prefetch path, and the host memory syscalls so every
   * query route has identical recall + promotion semantics (T5).
   *
   * M3: `memory_recalled` carries the kernel-derived count — the runner never computes
   * `recall_count + 1` itself. M4: `promotion_suggested` is advisory and already
   * edge-triggered by the kernel; the runner surfaces it and never auto-pins.
   */
  private async mirrorMemoryLifecycle(obs: KernelObservation): Promise<void> {
    if (obs.kind === "memory_recalled" && obs.recalls?.length) {
      const agentId = this.opts.agentId
      if (agentId && this.opts.dreamStore?.recordRecall) {
        await this.opts.dreamStore.recordRecall(agentId, obs.recalls)
      }
    }
    if (obs.kind === "promotion_suggested" && obs.record_id) {
      this.opts.onPromotionSuggested?.({
        recordId: obs.record_id,
        recallCount: obs.recall_count ?? 0,
      })
    }
  }

  private async consumeMemoryLifecycleObservations(
    sessionId: string | null | undefined,
    observations: KernelObservation[],
  ): Promise<void> {
    const turn = this.activeKernel?.turn() ?? 0
    for (const obs of observations) {
      if (!isMemoryLifecycleObservation(obs)) continue
      await this.mirrorMemoryLifecycle(obs)
      if (!sessionId) continue
      const event = kernelObservationToSessionEvent(obs, turn)
      if (event) await this.opts.sessionLog.append(sessionId, event)
    }
  }

  /** Mount a tool capability on the currently-running kernel runtime. No-op if not running. */
  async mountTool(schema: ToolSchema): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(capabilityTool(schema)))
  }

  /** Mount a skill capability on the currently-running kernel runtime. No-op if not running. */
  async mountSkill(name: string, description: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilitySkill({ name, description, estimatedTokens: 0 }),
    ))
  }

  /** Mount a generic marker capability (e.g. MCP server, agent) on the active run. No-op if not running. */
  async mountMarker(kind: string, id: string, description: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilityMarker(kind, id, description),
    ))
  }

  /** Unmount a capability by kind + id from the active run. No-op if not running. */
  async unmountCapability(kind: string, id: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, capabilityCommandUnmount(kind, id))
  }


  /** Push content into the Knowledge slot (memory retrievals, skill definitions, artifacts).
   *  K1: `opts.key` gives the entry identity — a same-key push upserts (applied at the next
   *  compaction/renewal boundary, where the cached system[1] block is rewritten anyway) instead
   *  of appending a duplicate. `opts.pinned` exempts the entry from the knowledge-budget sweep. */
  async pushKnowledge(message: Message, tokens?: number, opts?: { key?: string; pinned?: boolean }): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, {
      kind: "add_knowledge_message",
      content: message.content ?? "",
      tokens: tokens ?? Math.max(1, Math.ceil((message.content?.length ?? 0) / 4)),
      ...(opts?.key !== undefined ? { key: opts.key } : {}),
      ...(opts?.pinned ? { pinned: true } : {}),
    })
  }

  /** K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
   *  Errs-open: an unknown key is a kernel-side no-op. */
  async removeKnowledge(key: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, { kind: "remove_knowledge", key })
  }

  /** K3: host-driven skill deactivation (there is deliberately no model-facing unload — it
   *  invites thrash). The toolset re-widens at the next provider call; the skill's knowledge pin
   *  drops at the next compaction/renewal boundary. A later `skill(name)` call re-activates and
   *  re-pins fresh content. Errs-open: not-active is a kernel-side no-op. */
  async deactivateSkill(name: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, { kind: "skill_deactivated", name })
    // Re-arm the SDK-side push guard so a re-activation re-pins the content.
    this.knowledgePushedSkills.delete(name)
  }

  /**
   * Spawn an isolated sub-agent via the kernel, run it on the host, and feed the result back.
   * Requires an active parent run (`run()` / `wake()` in progress or paused at milestone).
   */
  async *spawnSubAgent(spec: AgentRunSpec): AsyncIterable<StreamEvent> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("spawnSubAgent requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel

    const observations = await this.commitKernelApply(runtime, this.pendingObservations, {
      kind: "spawn_sub_agent",
      spec: agentRunSpecToKernel(spec),
      parent_session_id: parentSessionId,
    })
    this.nextArchiveStart = await this.appendObservations(parentSessionId, runtime, this.nextArchiveStart)

    const spawned = findSpawnProcessObservation(observations)
    if (!spawned) {
      const rejected = controlRequestRejection(observations, "spawn_sub_agent")
      if (rejected) {
        yield { type: "error", message: `spawn_sub_agent denied: ${rejected.reason}` } as ErrorEvent
        return
      }
      throw new Error("spawn_sub_agent did not emit agent_process_changed")
    }

    const manifest = spawnObservationToManifest(spawned, spec, parentSessionId)

    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator
    const result = await orchestrator.run({
      parentOpts: this.opts,
      parentSessionId,
      spec,
      manifest,
      sessionLog: this.opts.sessionLog,
      ...(this.opts.subAgentHarness ? { harness: this.opts.subAgentHarness } : {}),
    })

    await this.commitKernelApply(runtime, this.pendingObservations, {
      kind: "sub_agent_completed",
      result: subAgentResultToKernel(result),
    })
    yield { type: "done", iterations: result.result.turnsUsed, totalTokens: result.result.totalTokensUsed, status: result.result.termination } as DoneEvent
  }

  /**
   * G3: run one workflow node, enforcing its `output_schema` (if any). Without a schema this is a
   * plain `orchestrator.run`. With one, the node's agent is instructed to emit conforming JSON, its
   * output is validated (the supported JSON-Schema subset), and on mismatch the node is re-run once
   * with the validation errors fed back. If it still does not conform, the node is failed with the
   * validation reason — a node that cannot meet its declared output contract starves its dependents,
   * exactly as a denied spawn does.
   */
  private async runWorkflowNode(
    node: WorkflowSpawnInfo,
    parentSessionId: string,
    orchestrator: SubAgentOrchestrator,
    budget?: WorkflowBudget,
    outputs?: Map<string, string>,
    abortSignal?: AbortSignal,
  ): Promise<SubAgentResult> {
    // G2: a reduce node runs no LLM — execute the registered pure function over its dependency
    // outputs and feed the result back as an ordinary completion. Deterministic; no agent burned.
    if (node.reducer) {
      return this.runReduceNode(node, outputs ?? new Map())
    }

    const baseSpec = workflowNodeToSpec(node, parentSessionId)
    const manifest = workflowNodeToManifest(node, parentSessionId)
    // G4: surface the workflow's remaining budget to the node's agent so a coordinator can size its
    // `submit_workflow_nodes` batch to what is available (empty string ⇒ unbounded, no note).
    const budgetNote = workflowBudgetNote(budget)
    // W-N2: a DAG edge carries data — every dependent node sees its dependencies' outputs (the
    // kernel sends `input_agent_ids` for all dependents; judges/reduce keep their special paths).
    const depsNote = dependencyOutputsNote(node.input_agent_ids, outputs)
    const withBudget = (goal: string) =>
      [goal, depsNote, budgetNote].filter(Boolean).join("\n\n")
    const mkCtx = (goal: string) => ({
      parentOpts: this.opts,
      parentSessionId,
      spec: { ...baseSpec, goal: withBudget(goal) },
      manifest,
      sessionLog: this.opts.sessionLog,
      // M5 v2.1: this child IS a workflow node — its `start_workflow` flattens to this kernel (the
      // workflow it would author joins the running DAG) rather than bootstrapping a nested pivot.
      isWorkflowNode: true,
      // W-N1: trusted workflow nodes run on the parent's execution plane (they carry no grant list
      // by design — filtering on the missing list ran every DAG node TOOL-LESS); quarantined nodes
      // stay deny-all filtered (they read untrusted content).
      toolAccess: (node.trust === "quarantined" ? "filtered" : "inherit") as "filtered" | "inherit",
      // #2-B-ii: the per-node abort signal the driver fires when the kernel preempts this node.
      ...(abortSignal ? { abortSignal } : {}),
      ...(this.opts.subAgentHarness ? { harness: this.opts.subAgentHarness } : {}),
    })
    const textOf = (r: SubAgentResult): string => {
      const c = r.result.finalMessage?.content
      return typeof c === "string" ? c : c != null ? JSON.stringify(c) : ""
    }
    const withSignal = (r: SubAgentResult, patch: Partial<SubAgentResult["result"]>): SubAgentResult =>
      ({ ...r, result: { ...r.result, ...patch } })

    // A#2 tournament judge: this node compares two entrants' produced outputs rather than running its
    // own goal. Look up both candidates, run a judge over the controller's criterion, and report the
    // winning entrant's agent id as `tournamentWinner` (the kernel advances the bracket with it).
    if (node.judge_match) {
      const out = outputs ?? new Map<string, string>()
      const left = out.get(node.judge_match.left) ?? ""
      const right = out.get(node.judge_match.right) ?? ""
      const result = await orchestrator.run(mkCtx(judgeGoal(baseSpec.goal, left, right)))
      const winner = extractJudgeWinner(textOf(result))
      const winnerId = winner === "right" ? node.judge_match.right : node.judge_match.left
      return withSignal(result, { tournamentWinner: winnerId })
    }

    // A#2 v2 loop iteration: run the increment under the armed pacing trap (workflowNodeToSpec set
    // `loopRound`, and the iteration resumes the loop's stable session — transcript-as-carry).
    // DW-3 one vocabulary: the kernel-adjudicated `pace` verb IS the continuation signal
    // (stop → loopContinue=false); the legacy text-sniffed JSON blob survives only as the fallback
    // when no pace decision arrives (stub orchestrators, harness children), where no signal still
    // means "run to max_iters" (v1).
    if (node.loop_max_iters != null) {
      const iteration = Number(/-i(\d+)$/.exec(node.agent_id)?.[1] ?? "0")
      const result = await orchestrator.run(
        mkCtx(`${baseSpec.goal}\n\n${loopInstruction(node.loop_max_iters, iteration)}`),
      )
      const pace = result.result.paceDecision
      if (pace) return withSignal(result, { loopContinue: pace.action !== "stop" })
      const cont = extractLoopContinue(textOf(result))
      return cont === undefined ? result : withSignal(result, { loopContinue: cont })
    }

    // A#2 classify: run the classifier, then extract the chosen branch label; the kernel runs that
    // branch and prunes the rest. No recognizable choice ⇒ leave unset (kernel prunes all branches).
    if (node.classify_labels && node.classify_labels.length) {
      const labels = node.classify_labels
      const result = await orchestrator.run(mkCtx(`${baseSpec.goal}\n\n${classifyInstruction(labels)}`))
      const branch = extractClassifyBranch(textOf(result), labels)
      return branch === undefined ? result : withSignal(result, { classifyBranch: branch })
    }

    const schema = node.output_schema
    if (!schema) return orchestrator.run(mkCtx(baseSpec.goal))

    const maxAttempts = this.opts.workflowSchemaValidationAttempts ?? 2
    let last: SubAgentResult | undefined
    let lastErrors: string[] = []
    for (let attempt = 1; attempt <= maxAttempts; attempt++) {
      const goal =
        attempt === 1
          ? `${baseSpec.goal}\n\n${schemaInstruction(schema)}`
          : `${baseSpec.goal}\n\n${schemaRetryInstruction(schema, lastErrors)}`
      const result = await orchestrator.run(mkCtx(goal))
      const content = result.result.finalMessage?.content
      const text = typeof content === "string" ? content : content != null ? JSON.stringify(content) : ""
      const v = validateAgainstSchema(extractJsonValue(text), schema)
      if (v.ok) return result
      last = result
      lastErrors = v.errors
    }

    const reason = `output_schema validation failed after ${maxAttempts} attempts: ${lastErrors.join("; ")}`
    const fallback = last as SubAgentResult
    return {
      ...fallback,
      result: {
        ...fallback.result,
        termination: "error",
        finalMessage: { role: "assistant", content: reason, toolCalls: [] },
      },
    }
  }

  /**
   * G2: execute a deterministic reduce node. Looks up the named reducer (built-ins overlaid with
   * `opts.reducers`), runs it over the node's dependency outputs (gathered from `outputs`), and
   * returns a synthetic completion carrying the reducer's output — no LLM, zero tokens. An unknown
   * reducer or a thrown reducer fails the node (`Error` termination → the kernel starves dependents).
   */
  private runReduceNode(node: WorkflowSpawnInfo, outputs: Map<string, string>): SubAgentResult {
    const ok = (content: string, termination: string): SubAgentResult => ({
      agentId: node.agent_id,
      result: { termination, finalMessage: { role: "assistant", content, toolCalls: [] }, turnsUsed: 0, totalTokensUsed: 0 },
    })
    const reducer = resolveReducer(node.reducer as string, this.opts.reducers)
    if (!reducer) return ok(`unknown reducer "${node.reducer}"`, "error")
    const inputs = (node.input_agent_ids ?? []).map(agentId => ({ agentId, output: outputs.get(agentId) ?? "" }))
    try {
      return ok(reducer(inputs), "completed")
    } catch (err) {
      return ok(`reducer "${node.reducer}" threw: ${formatToolError(err)}`, "error")
    }
  }

  /**
   * W0-ABI: run a declarative workflow DAG. The kernel owns the DAG and gates every node spawn
   * through the syscall trap; this driver runs each kernel-emitted batch of nodes in parallel,
   * feeds their results back, and loops until the kernel reports the workflow complete.
   * Returns one typed terminal outcome for every node in the DAG.
   */
  async runWorkflow(
    spec: WorkflowSpec,
    opts?: {
      /** Typed recovered terminal outcomes, including control signals and output. */
      resumedOutcomes?: RecoveredNodeOutcome[]
      resumedSubmissions?: Record<string, unknown>[][]
      /** R3-1: original base index per submission batch (parallel to resumedSubmissions). */
      resumedSubmissionBases?: number[]
      /** Standalone session id when bootstrapping (no active parent run). Defaults to a fresh uuid. */
      sessionId?: string
    },
  ): Promise<WorkflowOutcome> {
    // Standalone entry: with no active parent run (e.g. a stateless HTTP handler), auto-bootstrap a
    // kernel that owns the DAG — start_run + the same governance/quota/attention policies a full run
    // gets — then tear it down on completion so the runner is reusable. Mid-run callers (activeKernel
    // already set by an in-flight `run()`) keep the original in-place behavior with no teardown.
    const bootstrapped = !this.activeKernel || !this.currentSessionId
    let groupBudgetScope: GroupBudgetScope | undefined

    try {
      if (bootstrapped) {
        const sessionId = opts?.sessionId ?? `wf-${crypto.randomUUID()}`
        // A standalone workflow reserves a bounded slice before its kernel schedules any node.
        // Mid-run callers reuse their parent run's already-active reservation.
        if (this.opts.runGroup) {
          const g = this.opts.runGroup
          groupBudgetScope = await GroupBudgetScope.open(
            g,
            { sessionId, role: this.opts.agentId, kind: "vehicle" },
            this.groupBudgetRequest(false),
          )
          this.activeGroupBudgetScope = groupBudgetScope
        }
        // Resume depends on this fact. Do not dispatch any node until it is durable.
        await this.opts.sessionLog.append(sessionId, {
          kind: "run_started",
          run_id: crypto.randomUUID(),
          goal: `workflow:${spec.nodes.length} nodes`,
          criteria: [],
          agent_id: this.opts.agentId,
        })
        await this.bootstrapWorkflowKernel(sessionId, groupBudgetScope)
      }
      const parentSessionId = this.currentSessionId!
      const runtime = this.activeKernel!
      const observationStart = this.pendingObservations.length
      const initialAction = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
        kind: "load_workflow",
        spec: workflowSpecToKernel(spec),
        parent_session_id: parentSessionId,
        // Exact typed terminal outcomes plus control-flow signals recovered from the journal.
        ...(opts?.resumedOutcomes?.length
          ? {
              resumed_outcomes: opts.resumedOutcomes.map(r => ({
                agent_id: r.agentId,
                status: r.status,
                termination: r.termination,
                ...(r.output ? { output: messageToKernelMessage(r.output) } : {}),
                ...(r.classifyBranch !== undefined ? { classify_branch: r.classifyBranch } : {}),
                ...(r.tournamentWinner !== undefined ? { tournament_winner: r.tournamentWinner } : {}),
                ...(r.loopContinue !== undefined ? { loop_continue: r.loopContinue } : {}),
              })),
            }
          : {}),
        // R3-1: re-apply recorded runtime submissions so dynamically-appended nodes are reconstructed.
        ...(opts?.resumedSubmissions?.length ? { resumed_submissions: opts.resumedSubmissions } : {}),
        ...(opts?.resumedSubmissionBases?.length ? { resumed_submission_bases: opts.resumedSubmissionBases } : {}),
      })
      const observations = this.pendingObservations.slice(observationStart)
      const outcome = await this.driveWorkflow(
        initialAction,
        observations,
        parentSessionId,
        runtime,
        recoveredOutputs(opts?.resumedOutcomes),
      )
      if (bootstrapped) {
        const terminal = await this.commitKernelAction(runtime, this.pendingObservations, { kind: "complete_run" })
        if (terminal.kind !== "done") {
          throw new Error("complete_run did not produce a terminal kernel action")
        }
        await this.appendObservations(parentSessionId, runtime, 0)
      }
      return outcome
    } finally {
      if (bootstrapped) {
        try {
          if (groupBudgetScope && !groupBudgetScope.isClosed) {
            await groupBudgetScope.release()
          }
        } finally {
          this.activeKernel = null
          this.currentSessionId = null
          this.pendingObservations = []
          this.activeGroupBudgetScope = undefined
        }
      }
    }
  }

  /**
   * Bootstrap a standalone kernel for a host-driven workflow with NO active parent run — the path a
   * stateless request handler takes when it calls `runWorkflow(spec)` directly. Mirrors `execute()`'s
   * pre-run kernel setup (governance / attention / quota via `applyKernelPolicies`, then `start_run`)
   * after `runWorkflow` has durably recorded `run_started`. Sets `activeKernel` / `currentSessionId`;
   * `runWorkflow` is responsible for tearing them down.
   */
  private async bootstrapWorkflowKernel(
    sessionId: string,
    groupBudgetScope?: GroupBudgetScope,
  ): Promise<KernelRuntimeInstance> {
    this.interrupted = false
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId

    const runtime = this.createSyscallRuntime()
    this.activeKernel = runtime

    await this.applyKernelPolicies(runtime, groupBudgetScope)
    // ABI v2 has one lifecycle: standalone workflows start a real run before loading their DAG.
    // The initial provider effect is superseded by the workflow load; no self-bootstrap escape hatch.
    await this.commitKernelAction(runtime, this.pendingObservations, {
      kind: "start_run",
      task: { goal: `workflow session ${sessionId}`, criteria: [] },
    })
    return runtime
  }

  /**
   * M5/G1: bootstrap an **agent-authored** workflow ("the model writes its own harness"). Unlike
   * `runWorkflow` (the host fires the privileged `load_workflow`), this routes the spec through the
   * agent-reachable `Syscall::LoadWorkflow` (the `submit_workflow` event): with no workflow active the
   * kernel **bootstraps** the DAG; if one is already active it **flattens** the spec's nodes onto it
   * (bootstrap-or-flatten — one kernel, one quota, never a workflow stack). Gated by the same
   * `max_workflow_nodes` backstop as runtime submission, so an authored harness can't overgrow the run.
   * The resulting batches are driven by the same shared driver as `runWorkflow`.
   */
  async bootstrapWorkflow(
    spec: WorkflowSpec,
    opts?: { submitterAgentId?: string },
  ): Promise<WorkflowOutcome> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("bootstrapWorkflow requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel
    const observationStart = this.pendingObservations.length
    const initialAction = await this.commitKernelMaybeAction(
      runtime,
      this.pendingObservations,
      submitWorkflowToKernel(spec, parentSessionId, opts?.submitterAgentId),
    )
    const observations = this.pendingObservations.slice(observationStart)
    // W-3: persist the agent-authored batch (bootstrap base 0 / flatten base N — the kernel now
    // announces BOTH) so an interrupted authored workflow reconstructs on resume; the host never
    // had this spec, unlike the `runWorkflow` path.
    const submitted = observations.find(o => o.kind === "workflow_nodes_submitted") as
      | { base?: number }
      | undefined
    if (submitted) {
      await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
        turn: runtime.turn(),
        nodes: (workflowSpecToKernel(spec).nodes as Record<string, unknown>[]) ?? [],
        baseIndex: submitted.base,
        submitterAgentId: opts?.submitterAgentId,
      }))
    }
    return this.driveWorkflow(initialAction, observations, parentSessionId, runtime)
  }

  /**
   * M5 v2.1: drive the sub-workflow(s) a top-level agent authored via `start_workflow`. Called at the
   * verified-safe point (right after the tool turn resolved to `call_provider` — kernel in Reason, not
   * suspended). For each authored spec: `bootstrapWorkflow` runs it in THIS kernel (the kernel resumes
   * the agent reason loop on `workflow_completed` — `finish_workflow` sets phase=Reason), then the
   * outcome is injected as a user message so the agent's next turn sees the result. Returns a fresh
   * `call_provider` synthesized from the updated context (the workflow drive consumed its own kernel
   * actions, so we re-render — the same pattern as the reactive-compact retry path).
   */
  private async driveAuthoredWorkflows(
    runtime: KernelRuntimeInstance,
    action: Extract<KernelRunnerAction, { kind: "call_provider" }>,
  ): Promise<Extract<KernelRunnerAction, { kind: "call_provider" }>> {
    const specs = this.pendingAuthoredWorkflows
    this.pendingAuthoredWorkflows = []
    this.workflowContinuation = null
    for (const spec of specs) {
      await this.bootstrapWorkflow(spec)
    }
    const continuation = this.workflowContinuation
    if (!continuation) throw new Error("authored workflow completed without a provider continuation")
    return continuation
  }

  /**
   * #2-B-ii: while a workflow batch is in flight, poll the signal source. A Critical `InterruptNow`
   * routes through the kernel (which, with the root suspended in `SubAgentAwait`, preempts — marks the
   * running nodes `UserAbort`, tears the `WorkflowRun` down, emits `AgentPreempted`); we then abort the
   * matching children's in-flight LLM calls (via their per-node `AbortController` → `interrupt()`).
   * Returns the torn-down workflow's outcome on preemption, else `null`. No-op (null) without a signal
   * source. Non-preempting signals (queue/observe/soft-interrupt) are still applied as they arrive.
   */
  private async monitorWorkflowPreemption(
    runtime: KernelRuntimeInstance,
    controllers: Map<string, AbortController>,
    batchState: { settled: boolean },
  ): Promise<WorkflowNodeOutcome[] | null> {
    const source = this.opts.signalSource
    if (!source) return null
    while (!batchState.settled) {
      // O2: injected notes participate in the monitor too, so a host `injectNote` mid-batch is not
      // stranded until the batch settles (the drain order matches `nextInboundSignal`).
      const delivery = await this.nextInboundSignal()
      if (batchState.settled) {
        await delivery?.nack()
        break
      }
      if (!delivery) { await new Promise(resolve => setTimeout(resolve, 5)); continue }
      const observationStart = this.pendingObservations.length
      const signalAction = await this.consumeInboundSignal(delivery, sig =>
        this.commitKernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(sig)))
      let observations = this.pendingObservations.slice(observationStart)
      if (signalAction?.kind === "preempt_sub_agents") {
        for (const id of signalAction.agentIds) controllers.get(id)?.abort()
        const resultStart = this.pendingObservations.length
        const continuation = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
          kind: "preempt_result",
          effect_id: signalAction.effectId,
        })
        if (continuation && continuation.kind !== "call_provider" && continuation.kind !== "done") {
          throw new Error(`workflow preemption returned unexpected effect: ${continuation.kind}`)
        }
        observations = [...observations, ...this.pendingObservations.slice(resultStart)]
      } else if (signalAction) {
        throw new Error(`workflow signal returned unexpected effect: ${signalAction.kind}`)
      }
      const preempted = observations.find(o => o.kind === "agent_preempted") as { agent_ids?: string[] } | undefined
      if (preempted) {
        for (const id of preempted.agent_ids ?? []) controllers.get(id)?.abort()
        const wc = observations.find(o => o.kind === "workflow_completed") as
          | { node_outcomes?: KernelWorkflowNodeOutcome[] }
          | undefined
        return (wc?.node_outcomes ?? []).map(workflowNodeOutcomeFromKernel)
      }
    }
    return null
  }

  /**
   * Shared workflow driver for `runWorkflow` (host `load_workflow`) and `bootstrapWorkflow` (agent
   * `submit_workflow`): given the observations from the initial load/bootstrap, run each kernel-emitted
   * batch in parallel, feed completions back (appending any agent-submitted nodes first), and loop
   * until the kernel reports the workflow complete. Returns typed terminal node outcomes.
   */
  private async driveWorkflow(
    initialAction: KernelRunnerAction | null,
    initial: KernelObservation[],
    parentSessionId: string,
    runtime: KernelRuntimeInstance,
    seedOutputs?: Map<string, string>,
  ): Promise<WorkflowOutcome> {
    let observations = initial
    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator
    const findDone = (obs: typeof observations) =>
      obs.find(o => o.kind === "workflow_completed") as
        | { node_outcomes?: KernelWorkflowNodeOutcome[] }
        | undefined

    const acceptSpawn = async (
      spawn: Extract<KernelRunnerAction, { kind: "spawn_workflow" }>,
    ): Promise<KernelObservation[]> => {
      const observationStart = this.pendingObservations.length
      const continuation = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
        kind: "workflow_spawn_result",
        effect_id: spawn.effectId,
        started_agent_ids: spawn.nodes.map(node => String(node.agent_id ?? "")),
        failures: [],
      })
      if (continuation) {
        throw new Error(`workflow spawn acknowledgement returned unexpected effect: ${continuation.kind}`)
      }
      return this.pendingObservations.slice(observationStart)
    }

    let done = findDone(observations)
    if (done) {
      if (initialAction?.kind === "call_provider") this.workflowContinuation = initialAction
      return { nodeOutcomes: (done.node_outcomes ?? []).map(workflowNodeOutcomeFromKernel), outputs: {} }
    }
    if (!initialAction) return { nodeOutcomes: [], outputs: {} }
    const workflowRejection = controlRequestRejection(observations)
    if (initialAction.kind === "call_provider" && workflowRejection) {
      this.workflowContinuation = initialAction
      return { nodeOutcomes: [], outputs: {}, rejection: workflowRejection }
    }
    if (initialAction.kind !== "spawn_workflow") {
      throw new Error(`workflow load returned unexpected kernel effect: ${initialAction.kind}`)
    }
    let nodes = initialAction.nodes as unknown as WorkflowSpawnInfo[]
    let budget = initialAction.budget as unknown as WorkflowBudget | undefined
    observations = await acceptSpawn(initialAction)
    done = findDone(observations)
    // G2: each completed node's output, keyed by agent id — a reduce node reads its dependencies'
    // outputs from here. Deps always complete in an earlier round than the reduce node that needs
    // them (the kernel keeps the reduce node un-ready until its deps finish), so this is populated.
    // W-1: on resume it is pre-seeded from the persisted node outputs, so post-resume dependents
    // still see their (pre-crash) dependencies' outputs.
    const outputs = new Map<string, string>(seedOutputs ?? [])

    for (;;) {
      if (nodes.length === 0) return { nodeOutcomes: [], outputs: Object.fromEntries(outputs) } // nothing to run (e.g. all gated)

      // Run the currently-runnable nodes in parallel — each is independent within a round.
      const roundBudget = budget
      // #2-B-ii: per-node abort controllers + a concurrent preemption monitor. While the batch is in
      // flight the monitor polls the signal source; a Critical `InterruptNow` routes through the kernel
      // (which preempts → `AgentPreempted` + tears the workflow down) and we abort the matching child's
      // in-flight LLM call. If the kernel preempted, stop driving and return the torn-down outcome.
      const controllers = new Map(nodes.map(n => [n.agent_id, new AbortController()] as const))
      const batchState = { settled: false }
      const monitor = this.monitorWorkflowPreemption(runtime, controllers, batchState)
      const results = await Promise.all(
        nodes.map(node => this.runWorkflowNode(node, parentSessionId, orchestrator, roundBudget, outputs, controllers.get(node.agent_id)?.signal)),
      )
      batchState.settled = true
      const preempted = await monitor
      if (preempted) return { nodeOutcomes: preempted, outputs: Object.fromEntries(outputs) }

      // Feed completions back one at a time. The kernel's run-queue executor may spawn a node's
      // dependents the moment *that* node completes (per-node unblock), so each feed can emit its
      // own `workflow_batch_spawned`; ACCUMULATE them across the round rather than keeping only the
      // last feed's (the old code overwrote `observations` per feed and dropped nodes unblocked by
      // earlier completions — stalling uneven DAGs). Completion and new spawns are mutually
      // exclusive per feed, so a `workflow_completed` only arrives once nothing remains to run.
      const nextNodes: WorkflowSpawnInfo[] = []
      done = undefined
      for (const result of results) {
        // G2: record this node's output so a downstream reduce node can consume it.
        const outContent = result.result.finalMessage?.content
        const outText = typeof outContent === "string" ? outContent : outContent != null ? JSON.stringify(outContent) : ""
        outputs.set(result.agentId, outText)
        // A loop iteration completes under `wf-node{N}-i{k}` but its dependents consume the STABLE
        // node id `wf-node{N}` — alias it so the LAST iteration's output is what dependents see.
        const stableId = result.agentId.replace(/-i\d+$/, "")
        if (stableId !== result.agentId) outputs.set(stableId, outText)
        // R3-1: if this node's agent submitted more nodes, append them to the parent DAG BEFORE
        // reporting the node's completion — the workflow is still active (the kernel hasn't seen this
        // node finish), so even a submission from the last running node keeps the DAG alive. The
        // appended nodes' `workflow_batch_spawned` is collected into this round like any other.
        if (result.submittedNodes?.length) {
          // G1: stamp the submitting node's agent id so the kernel can coerce a quarantined
          // submitter's nodes to quarantined (no topological privilege escalation).
          const submitEvent = submitWorkflowNodesToKernel(result.submittedNodes, result.agentId)
          const observationStart = this.pendingObservations.length
          const submitAction = await this.commitKernelMaybeAction(runtime, this.pendingObservations, submitEvent)
          const subObs = this.pendingObservations.slice(observationStart)
          const rejected = controlRequestRejection(subObs, "submit_workflow_nodes")
            ?? (subObs.find(o => o.kind === "nodes_rejected")
              ? { operation: "submit_workflow_nodes", reason: String(subObs.find(o => o.kind === "nodes_rejected")?.reason ?? "request denied") }
              : undefined)
          if (rejected) {
            const denial = `workflow node submission denied: ${rejected.reason}`
            result.result = {
              ...result.result,
              termination: "error",
              finalMessage: { role: "assistant", content: denial, toolCalls: [] },
            }
            outputs.set(result.agentId, denial)
            if (stableId !== result.agentId) outputs.set(stableId, denial)
          }
          if (submitAction?.kind === "spawn_workflow") {
            nextNodes.push(...submitAction.nodes as unknown as WorkflowSpawnInfo[])
            budget = submitAction.budget as unknown as WorkflowBudget | undefined ?? budget
            const accepted = await acceptSpawn(submitAction)
            const submittedDone = findDone([...subObs, ...accepted])
            if (submittedDone) done = submittedDone
          } else if (submitAction) {
            throw new Error(`workflow node submission returned unexpected effect: ${submitAction.kind}`)
          }
          // R3-1: persist the submission (kernel-shape nodes) + its kernel-reported base index
          // so resume can re-apply the batch at the exact original graph position. W-N3: also the
          // submitter, so resume drops batches whose submitter re-runs (it will re-submit).
          const submitted = subObs.find(o => o.kind === "workflow_nodes_submitted") as
            | { base?: number }
            | undefined
          if (submitted) {
            await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
              turn: runtime.turn(),
              nodes: (submitEvent.nodes as Record<string, unknown>[]) ?? [],
              baseIndex: submitted.base,
              submitterAgentId: result.agentId,
            }))
          }
        }
        const observationStart = this.pendingObservations.length
        const completionAction = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
          kind: "sub_agent_completed",
          result: subAgentResultToKernel(result),
        })
        let obs = this.pendingObservations.slice(observationStart)
        if (completionAction?.kind === "spawn_workflow") {
          nextNodes.push(...completionAction.nodes as unknown as WorkflowSpawnInfo[])
          budget = completionAction.budget as unknown as WorkflowBudget | undefined ?? budget
          obs = [...obs, ...await acceptSpawn(completionAction)]
        } else if (completionAction?.kind === "call_provider") {
          this.workflowContinuation = completionAction
        } else if (completionAction) {
          throw new Error(`workflow completion returned unexpected effect: ${completionAction.kind}`)
        }
        const d = findDone(obs)
        if (d) done = d
        // Persist node completion for resume recovery. W-1: the result-borne control signals ride
        // along (a resumed classifier re-prunes; a recorded loop stop is honored) plus the output
        // text (post-resume dependents/reduce still see this node's output).
        await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodeCompletedEvent({
          turn: runtime.turn(),
          agentId: result.agentId,
          status: workflowNodeStatusFromTermination(result.result.termination),
          termination: result.result.termination,
          classifyBranch: result.result.classifyBranch,
          tournamentWinner: result.result.tournamentWinner,
          loopContinue: result.result.loopContinue,
          ...(result.result.finalMessage ? { output: result.result.finalMessage } : {}),
        }))
      }
      if (done && nextNodes.length === 0) {
        return {
          nodeOutcomes: (done.node_outcomes ?? []).map(workflowNodeOutcomeFromKernel),
          outputs: Object.fromEntries(outputs),
        }
      }
      nodes = nextNodes
    }
  }

  /**
   * Resume a workflow from the parent session's completed nodes.
   * Reads the session log, extracts completed workflow node records (with their W-1 control
   * signals + outputs), and calls runWorkflow so the kernel skips those nodes, replays control
   * flow (classify prune / loop stop), and the driver re-seeds its outputs map.
   */
  async resumeWorkflow(
    spec: WorkflowSpec,
    opts?: { sessionId?: string },
  ): Promise<WorkflowOutcome> {
    // Standalone resume: a stateless handler passes the prior `sessionId` to pick up an interrupted
    // workflow from the session log. Mid-run callers omit it and resume the active session.
    const sessionId = opts?.sessionId ?? this.currentSessionId
    if (!sessionId) {
      throw new Error("resumeWorkflow requires an active parent run or an explicit sessionId")
    }
    const events = await this.opts.sessionLog.read(sessionId)
    const resumedOutcomes = recoverWorkflowNodeOutcomes(events)
    const completedIds = new Set(resumedOutcomes.map(r => r.agentId))
    const recovered = recoverSubmittedWorkflowNodes(events)
    // W-N3: DROP batches whose submitter did NOT complete — that node re-runs on resume and will
    // re-submit its batch; replaying the logged copy too would duplicate its nodes in the DAG.
    // Exact bases keep later graph indices stable while dropped slots remain inert placeholders.
    let { submissions, bases } = recovered
    if (submissions.length > 0) {
      const keep = recovered.submitters.map(s => s === undefined || completedIds.has(s))
      submissions = submissions.filter((_, i) => keep[i])
      bases = bases.filter((_, i) => keep[i])
    }
    return this.runWorkflow(spec, {
      resumedOutcomes,
      resumedSubmissions: submissions,
      resumedSubmissionBases: bases,
      sessionId,
    })
  }

  interrupt(reason: OperationCancellationReason = "user"): void {
    this.interrupted = true
    this.cancellationReason = reason
    this.abortController?.abort(reason)
  }

  /** Push a contextual note into the run's signal stream (the system-reminder channel): it drains at
   *  the next turn boundary, routes through the kernel attention policy, and renders once as a
   *  `[SIGNAL] <text>` line in the volatile state turn. Use it to feed
   *  host-detected events back to the model mid-run (e.g. "that write was a no-op — stop repeating it")
   *  without wiring a full `SignalSource`. `urgency` maps to the kernel disposition ladder: `"normal"`
   *  queues for the next boundary (default), `"high"` soft-interrupts, `"critical"` preempts. */
  injectNote(text: string, urgency: RuntimeSignalUrgency = "normal"): void {
    this.injectedSignals.push({
      source: "custom",
      signalType: "event",
      urgency,
      payload: { goal: text },
    })
  }

  /** The most recent kernel session-entropy sample (one per completed turn), or `null` before the
   *  first boundary. A pull companion to the streamed `entropy_sample` events — hosts polling from
   *  outside the stream (e.g. a heartbeat supervisor) read the latest measurement here. */
  latestEntropy(): EntropySample | null {
    return this.lastEntropySample
  }

  /** Injected-note drain shared by the main loop's per-turn poll: injected notes first (FIFO), then
   *  the configured `signalSource`. Keeps the two inbound channels on one code path so they never drift. */
  private async nextInboundSignal(): Promise<InboundSignalDelivery | null> {
    const injected = this.injectedSignals.shift()
    if (injected) return {
      signalId: crypto.randomUUID(),
      deliveryId: `injected-${crypto.randomUUID()}`,
      deliveryAttempt: 1,
      signal: injected,
      ack: async () => true,
      nack: async () => true,
    }
    if (!this.opts.signalSource) return null
    const source = this.opts.signalSource
    const claim = await source.claimSignal(this.currentSessionId ?? undefined)
    if (!claim) return null
    const receipt: SignalDeliveryReceipt = {
      deliveryId: claim.deliveryId,
      leaseToken: claim.leaseToken,
    }
    return {
      signalId: claim.signalId,
      deliveryId: claim.deliveryId,
      deliveryAttempt: claim.deliveryAttempt,
      signal: claim.signal,
      ack: () => source.ackSignal(receipt),
      nack: () => source.nackSignal(receipt),
    }
  }

  private async consumeInboundSignal<T>(
    delivery: InboundSignalDelivery,
    consume: (delivery: InboundSignalDelivery) => Promise<T> | T,
  ): Promise<T> {
    try {
      const observationStart = this.pendingObservations.length
      const result = await consume(delivery)
      const dispositions = this.pendingObservations.slice(observationStart).filter(observation =>
        observation.kind === "signal_delivery_disposed"
        && observation.delivery_id === delivery.deliveryId
        && observation.attempt === delivery.deliveryAttempt)
      if (dispositions.length !== 1) {
        throw new Error("kernel did not return the matching signal delivery disposition")
      }
      if (!await delivery.ack()) throw new Error("signal lease was lost before acknowledgement")
      return result
    } catch (cause) {
      await delivery.nack()
      throw cause
    }
  }

  async *run(req: {
    sessionId: string
    goal: string
    criteria?: string[]
    /** Multimodal inputs (images / audio) attached to the task as a user message. */
    attachments?: ContentPart[]
    extensions?: Record<string, unknown>
    /** Parent transcript to preload (e.g. sub-agent full context inheritance). */
    inheritEvents?: Array<{ seq: number; event: SessionEvent }>
  }): AsyncIterable<StreamEvent> {
    const prior = req.inheritEvents ?? await this.opts.sessionLog.read(req.sessionId)
    const midRun = isMidRun(prior)
    const resumedStart = [...prior].reverse().find(entry => entry.event.kind === "run_started")
    const runId = midRun && resumedStart?.event.kind === "run_started"
      ? resumedStart.event.run_id
      : crypto.randomUUID()
    // Idempotent per session: an earlier run's `run_started` already carries these attachments
    // (same-session retry attempt), so replay reconstructs them — recording and seeding again
    // would double them in history. Deduping at the append keeps live and replay in agreement.
    const attachments = req.attachments?.length && !attachmentsAlreadySeeded(prior, req.attachments)
      ? req.attachments
      : undefined
    if (!midRun) {
      await this.opts.sessionLog.append(req.sessionId, {
        kind: "run_started",
        run_id: runId,
        goal: req.goal,
        criteria: req.criteria ?? [],
        agent_id: this.opts.agentId,
        system_prompt: this.opts.systemPrompt,
        ...(attachments ? { attachments } : {}),
      })
    }
    yield* this.execute(
      req.sessionId,
      req.goal,
      req.criteria ?? [],
      req.extensions,
      prior.length > 0 ? prior : undefined,
      midRun,
      attachments,
      runId,
    )
  }

  async *wake(sessionId: string, extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const events = await this.opts.sessionLog.read(sessionId)
    if (events.some(e => e.event.kind === "run_terminal")) return

    const startEntry = [...events].reverse().find(e => e.event.kind === "run_started")
    if (!startEntry) throw new Error(`No run_started event for session: ${sessionId}`)
    const start = startEntry.event as Extract<SessionEvent, { kind: "run_started" }>

    yield* this.execute(sessionId, start.goal, start.criteria, extensions, events, true, start.attachments, start.run_id)
  }

  /** Execute a kernel-owned approval effect and return the correlated decision lists. */
  private async resolveApprovalRequests(
    requests: Array<{ callId: string; tool: string; arguments: string; reason: string }>,
    runtime: KernelRuntimeInstance,
    sessionId: string,
  ): Promise<{ approved: string[]; denied: string[]; events: StreamEvent[] }> {
    const approved: string[] = []
    const denied: string[] = []
    const events: StreamEvent[] = []
    const runCtx: RunContext = { onPermissionRequest: this.opts.onPermissionRequest }

    for (const approval of requests) {
      const request: PermissionRequestEvent = {
        type: "permission_request",
        callId: approval.callId,
        toolName: approval.tool,
        arguments: approval.arguments,
        reason: approval.reason,
      }
      events.push(request)
      const decision = await resolvePermissionRequest(request, runCtx)
      events.push({
        type: "permission_resolved",
        callId: approval.callId,
        toolName: approval.tool,
        approved: decision.approved,
        responder: decision.responder ?? "host",
        ...(decision.reason ? { reason: decision.reason } : {}),
      } as PermissionResolvedEvent)
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_requested",
        turn: runtime.turn(),
        tool: approval.tool,
        arguments: approval.arguments,
        reason: request.reason,
      })
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_resolved",
        turn: runtime.turn(),
        approved: decision.approved,
        responder: decision.responder ?? "host",
      })
      if (decision.approved) {
        approved.push(approval.callId)
      } else {
        denied.push(approval.callId)
        const denyReason = decision.reason ?? "permission denied"
        events.push({
          type: "tool_denied",
          callId: approval.callId,
          toolName: approval.tool,
          reason: denyReason,
        } as ToolDeniedEvent)
        events.push({
          type: "tool_result",
          callId: approval.callId,
          name: approval.tool,
          content: `permission denied: ${denyReason}`,
          isError: true,
          errorKind: "governance_denied",
        } as ToolResultEvent)
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_denied",
          turn: runtime.turn(),
          call_id: approval.callId,
          tool_name: approval.tool,
          reason: denyReason,
        })
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: [{
            call_id: approval.callId,
            output: `permission denied: ${denyReason}`,
            is_error: true,
            error_kind: "governance_denied",
          }],
        })
      }
    }

    return { approved, denied, events }
  }

  /**
   * O7: resolve a `read_result` meta-tool call to the full text of a previously-evicted tool
   * output. Resolution order: (a) the on-disk result spool committed by the explicit
   * `spool_large_result` host effect, then (b) a session-log scan for the original
   * `tool_completed` event carrying that `call_id`. Slices the
   * resolved text by `[offset, offset + maxBytes)` (plain string slice — "bytes-ish").
   */
  private async resolveReadResult(
    sessionId: string,
    argsJson: string,
  ): Promise<{ text: string; isError: boolean }> {
    let callId = ""
    let offset = 0
    let maxBytes = 4000
    try {
      const args = JSON.parse(argsJson || "{}") as { call_id?: string; offset?: number; max_bytes?: number }
      callId = typeof args.call_id === "string" ? args.call_id : ""
      if (typeof args.offset === "number" && Number.isFinite(args.offset)) offset = args.offset
      if (typeof args.max_bytes === "number" && Number.isFinite(args.max_bytes)) maxBytes = args.max_bytes
    } catch {
      // malformed arguments — callId stays empty, falls through to "not found" below
    }

    let full: string | undefined
    const spool = this.opts.resultSpool ?? new LargeResultSpool()
    try {
      full = await spool.findByCallId(sessionId, callId)
    } catch {
      full = undefined
    }

    if (full === undefined) {
      try {
        const events = await this.opts.sessionLog.read(sessionId)
        for (const { event } of events) {
          if (event.kind !== "tool_completed") continue
          const match = event.results.find(r => r.call_id === callId)
          if (match) full = match.output
        }
      } catch {
        full = undefined
      }
    }

    if (full === undefined) {
      return { text: `no stored output for call_id "${callId}"`, isError: true }
    }

    const start = Math.max(0, offset)
    const end = Math.min(full.length, start + Math.max(0, maxBytes))
    const slice = full.slice(start, end)
    return {
      text: `[read_result ${callId}: chars ${start}–${end} of ${full.length}]\n${slice}`,
      isError: false,
    }
  }

  private async *execute(
    sessionId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
    attachments?: ContentPart[],
    runId: string = crypto.randomUUID(),
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.cancellationReason = undefined
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId
    if (this.opts.enableDiagnosticsDashboard) {
      this.dashboard = new KernelPrimitivesDashboard(sessionId)
    }
    const kernel = getKernel()
    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    const providerPolicy = this.opts.provider.runtimePolicy?.() ?? {}
    const effectiveMaxTurns = this.opts.maxTurns ?? providerPolicy.maxTurns ?? 25
    const effectiveTimeoutMs = this.opts.timeoutMs ?? providerPolicy.timeoutMs
    const operation: OperationContext = {
      runId,
      sessionId,
      agentId: this.opts.agentId,
      signal: this.abortController.signal,
      ...(effectiveTimeoutMs !== undefined ? { deadlineMs: Date.now() + effectiveTimeoutMs } : {}),
    }
    const taskScope = new ManagedTaskScope(operation, this.opts.onBackgroundTaskError)
    let groupBudgetScope: GroupBudgetScope | undefined

    try {
    const runtime = new kernel.KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: effectiveMaxTurns,
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
      maxTotalTokens: this.opts.maxTotalTokens !== undefined ? BigInt(this.opts.maxTotalTokens) : undefined,
    })
    this.activeKernel = runtime
    this.nextArchiveStart = nextCompressedArchiveStart

    if (this.opts.tokenizer) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_tokenizer",
        name: this.opts.tokenizer,
      })
    }
    if (this.opts.enablePlanTool !== undefined) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_plan_tool_enabled",
        enabled: this.opts.enablePlanTool,
      })
    }

    await this.commitKernelApply(runtime, this.pendingObservations, {
      kind: "set_tools",
      tools: this.opts.executionPlane.schemas().map(toolSchemaToKernel),
    })

    if (this.opts.systemPrompt) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "add_system_message",
        content: this.opts.systemPrompt,
        tokens: Math.max(1, Math.ceil(this.opts.systemPrompt.length / 4)),
      })
    }

    if (this.opts.initialMemory) {
      for (const mem of this.opts.initialMemory) {
        await this.commitKernelApply(runtime, this.pendingObservations, {
          kind: "add_knowledge_message",
          content: mem,
          tokens: Math.max(1, Math.ceil(mem.length / 4)),
        })
      }
    }

    if (this.opts.skillDir) {
      const { scanSkillDir } = await import("../skills/loader.js")
      const metas = await scanSkillDir(this.opts.skillDir)
      // P1-B: pass the full SkillMetadata (incl. `allowedTools`) straight through — re-mapping it
      // field-by-field previously dropped `allowedTools`.
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_available_skills",
        skills: metas.map(m => skillMetadataToKernel(m)),
      })
    }

    // P1-B/D: configure the stable-core tool ids (always exposed under skill gating). Empty/absent
    // ⇒ skills narrow to exactly their declared tools + meta-tools.
    if (this.opts.stableCoreToolIds?.length) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_stable_core_tools",
        tool_ids: this.opts.stableCoreToolIds,
      })
    }

    if (this.opts.dreamStore && this.opts.agentId) {
      await this.commitKernelApply(runtime, this.pendingObservations, { kind: "set_memory_enabled", enabled: true })
    }
    // Install optional memory policy. Maps the ergonomic camelCase option onto the kernel's
    // snake_case `set_memory_policy` event; omitted fields fall back to kernel defaults.
    if (this.opts.memoryPolicy) {
      const m = this.opts.memoryPolicy
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_memory_policy",
        ...(m.memoryPath !== undefined ? { memory_path: m.memoryPath } : {}),
        ...(m.staleWarningDays !== undefined ? { stale_warning_days: m.staleWarningDays } : {}),
        ...(m.retrievalTopK !== undefined ? { retrieval_top_k: m.retrievalTopK } : {}),
        ...(m.validationEnabled !== undefined ? { validation_enabled: m.validationEnabled } : {}),
        ...(m.maxContentBytes !== undefined ? { max_content_bytes: m.maxContentBytes } : {}),
        ...(m.maxNameLength !== undefined ? { max_name_length: m.maxNameLength } : {}),
        ...(m.promotionRecallThreshold !== undefined
          ? { promotion_recall_threshold: m.promotionRecallThreshold }
          : {}),
      })
    }
    if (this.opts.knowledgeSource) {
      await this.commitKernelApply(runtime, this.pendingObservations, { kind: "set_knowledge_enabled", enabled: true })
    }

    if (this.opts.milestoneContract) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "load_milestone_contract",
        contract: {
          phases: this.opts.milestoneContract.phases.map(p => ({
            id: p.id,
            criteria: p.criteria ?? [],
            unlocks: p.unlocks ?? [],
            required_evidence: p.requiredEvidence ?? [],
            ...(p.verifier ? { verifier: p.verifier } : {}),
          })),
        },
      })
    }

    const maxBytes = runtime.recoveryContentBytes()

    if (priorEvents && priorEvents.length > 0) {
      const repaired = repairEventsForRecovery(priorEvents, maxBytes)
      seedProviderReplayFromEvents(this.opts.provider, repaired)
      const loadArchive = this.opts.compressionStore
        ? (ref: string) => this.opts.compressionStore!.read(ref)
        : undefined
      const replayed = await replayMessagesAsync(repaired, maxBytes, loadArchive)
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayed.map(messageToKernelMessage),
      })
      // P1-B B3: rebuild active-skill gating after a wake by re-emitting SkillActivated for each
      // `skill` tool call in the replayed history (active_skills is not snapshotted — graceful).
      // The catalog (set_available_skills) was already fed above, so allowed_tools resolves.
      // `knowledge` isn't snapshotted either (same graceful-reset philosophy) — best-effort re-push
      // the skill's content from its replayed tool_result so the durable copy survives a wake too.
      const toolResultByCallId = new Map<string, string>()
      for (const m of replayed) {
        for (const part of m.contentParts ?? []) {
          if (part.type === "tool_result") toolResultByCallId.set(part.callId, part.output)
        }
      }
      for (const m of replayed) {
        for (const tc of m.toolCalls ?? []) {
          if (tc.name !== "skill") continue
          try {
            const name = (JSON.parse(tc.arguments || "{}") as { name?: string }).name
            if (!name) continue
            await this.commitKernelApply(runtime, this.pendingObservations, {
              kind: "skill_activated",
              name,
              ...(this.opts.skillLeaseTurns !== undefined ? { lease_turns: this.opts.skillLeaseTurns } : {}),
            })
            const output = toolResultByCallId.get(tc.id)
            if (output && !this.knowledgePushedSkills.has(name)) {
              this.knowledgePushedSkills.add(name)
              // K1: keyed — the kernel-side upsert is the authoritative dedup, so a wake re-push
              // of a skill already pinned live can never double-pin (the in-run Set resets with
              // each runner instance; the key does not).
              await this.pushKnowledge(
                { role: "system", content: output, toolCalls: [] },
                undefined,
                { key: `skill:${name}` },
              )
            }
          } catch { /* malformed skill args — skip */ }
        }
      }
    }

    const sessionStart = Date.now()
    const startPayload: Record<string, unknown> = {
      kind: "start_run",
      task: { goal, criteria },
    }
    // P0-A: lower an explicit `runSpec` and/or the `allowedToolIds` profile to the kernel's
    // `capability_filter`. `allowedToolIds` augments an explicit spec's filter, else synthesizes
    // a minimal top-level spec carrying just the filter (reuses the existing run_spec wire — no
    // new ABI). Unset on both ⇒ no run_spec ⇒ no gating (铁律: no config = old behavior).
    const allowedToolIds = this.opts.allowedToolIds
    const hasProfile = allowedToolIds !== undefined && allowedToolIds.length > 0
    if (this.opts.runSpec || hasProfile) {
      const baseSpec: AgentRunSpec = this.opts.runSpec ?? {
        identity: { agentId: this.opts.agentId ?? "root", sessionId, isSubAgent: false },
        role: "custom",
        goal,
      }
      const spec: AgentRunSpec = hasProfile
        ? { ...baseSpec, capabilityFilter: { ...baseSpec.capabilityFilter, allowedIds: allowedToolIds } }
        : baseSpec
      startPayload.run_spec = agentRunSpecToKernel(spec)
    }
    // Reserve capacity before start_run. The kernel enforces only this vehicle's grant and reports
    // exact terminal usage against the same opaque reservation identity.
    if (this.opts.runGroup) {
      const g = this.opts.runGroup
      groupBudgetScope = await GroupBudgetScope.open(
        g,
        { sessionId, role: this.opts.agentId, kind: "vehicle" },
        this.groupBudgetRequest(),
      )
      this.activeGroupBudgetScope = groupBudgetScope
    }
    await this.applyKernelPolicies(runtime, groupBudgetScope)
    // Multimodal upload: seed the user's attachments (images/audio) as a history
    // message before start_run pushes the "[TASK STATE]" anchor. init_task does not
    // clear history, so order becomes [attachment user msg, "Proceed…"] — both land
    // in the first render. On resume the message is already in the replayed history.
    if (!resumeMidRun && attachments?.length) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: attachmentsToKernelMessage(attachments),
      })
    }
    this.currentGoal = goal

    let action: KernelRunnerAction = resumeMidRun
      ? await this.commitKernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : await this.commitKernelAction(runtime, this.pendingObservations, startPayload)
    // I4/T5: pre-fetch memory before the first LLM turn so the model sees it on turn 1 instead
    // of discovering it via the `memory` tool on turn 3+. It routes through the kernel's
    // query_memory lifecycle and therefore runs AFTER start_run — a memory query is a kernel
    // preload whose result resumes the reasoning path, so issuing it pre-run would leave the
    // kernel Running and start_run would fault. The resumed action from the last query
    // supersedes start_run's (same pull contract; it renders the injected hits). Hits land in
    // `history` as ordinary turns — single-use retrieval content that decays with the
    // compression pyramid, never pinned into `knowledge`. Skipped on resumes (already in
    // prior context) and when dreamStore/agentId is absent.
    if (!resumeMidRun) {
      const resumed = await this.prefetchMemoryIntoHistory(runtime, "initial")
      if (resumed) action = resumed
    }
    // P0-C: the skill loaded and in effect going into the current turn (updated when the model's
    // `skill` tool call resolves). Drives the per-turn `activeSkill` metric → dwell measurement.
    let activeSkill: string | undefined

    // I0b: wrap the main loop so any uncaught kernel exception (typically a NAPI
    // Status::InvalidArg from a malformed input — e.g. RuntimeSignal.source with a wrong shape,
    // or an unrecognized event kind) is observable rather than silently propagating out of the
    // async generator. Without this wrap the runner emits no `run_terminal` event, so downstream
    // observability (session log, bench mechanism hooks) can't distinguish "the kernel rejected
    // an input" from "the run is still in progress."
    try {
    while (!runtime.isTerminal()) {
      nextCompressedArchiveStart = await this.appendObservations(
        sessionId,
        runtime,
        nextCompressedArchiveStart,
        taskScope,
      )
      this.nextArchiveStart = nextCompressedArchiveStart
      if (this.interrupted) {
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "cancel_operation",
          reason: this.cancellationReason ?? "user",
          pending_call_ids: pendingCallIds(action),
        })
        break
      }

      if (this.opts.signalSource || this.injectedSignals.length > 0) {
        const delivery = await this.nextInboundSignal()
        if (delivery) {
          // Kernel-routed: the kernel decides disposition (dedup/queue/interrupt) and emits
          // `signal_delivery_disposed`. An actionable disposition yields a new action to adopt; queued/observed/
          // ignored yields none (kernel buffers).
          const sigAction = await this.consumeInboundSignal(delivery, sig =>
            this.commitKernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(sig)))
          if (sigAction) action = sigAction
          // A critical signal is a kernel attention/preemption decision, not operation cancellation.
        }
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        // M5 v2.1: top-level auto-pivot at the safe point. If the agent authored sub-workflow(s) via
        // `start_workflow`, drive each in THIS kernel now (the kernel is in Reason / `call_provider`,
        // NOT suspended — driving mid-suspend would clobber the single-slot suspend state), inject the
        // outcome into context, and re-render. Loop-top placement (vs only after `tool_results`) catches
        // EVERY path to `call_provider` — including resuming after an approval gate — so a queued spec
        // is never stranded. Drains the queue; fires once per authored batch.
        if (this.pendingAuthoredWorkflows.length > 0) {
          action = await this.driveAuthoredWorkflows(runtime, action)
        }
        const providerEffectId = action.effectId
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        // I5: governance schema-level pre-filter. When a declarative GovernancePolicy is loaded
        // and `surfaceDeniedInSystem !== false`, drop denied tools from the schema BEFORE the
        // model sees them — the model can't plan a call it doesn't know about, so the rollback
        // overhead disappears. The list of denied names is appended to systemKnowledge so the
        // model knows not to plan around them.
        let context = action.context
        let tools = action.tools
        if (this.opts.governancePolicy && this.opts.governancePolicy.surfaceDeniedInSystem !== false) {
          const { allowed, denied } = governanceFilterSchema(tools, this.opts.governancePolicy)
          if (denied.length > 0) {
            tools = allowed
            const note = `[governance] the following tools are denied for this run and will fail if called: ${denied.join(", ")}.`
            context = {
              ...context,
              systemKnowledge: context.systemKnowledge
                ? `${context.systemKnowledge}\n\n${note}`
                : note,
            }
          }
        }
        let turnTokens = 0
        let turnInputTokens = 0
        let turnOutputTokens = 0
        let turnCacheReadTokens = 0
        let turnCacheCreationTokens = 0
        let turnCacheReadBySlot: { system?: number; tools?: number; messages?: number } | undefined
        let turnStopReason: string | undefined

        const abortSignal = this.abortController?.signal
        try {
          for await (const evt of this.opts.provider.stream(context, tools, Object.keys(ext).length ? ext : undefined, providerState, abortSignal)) {
            // #2-B-ii: a preempting `interrupt()` fires `abortController` — stop consuming the live
            // stream immediately (providers that forward `signal` also abort the socket; the rest at
            // least stop here at the next event). The loop-top `interrupted` check then ends the run.
            if (abortSignal?.aborted) break
            if (evt.type === "usage") {
              const usageEvt = evt as { type: string; totalTokens: number; inputTokens?: number; outputTokens?: number; cacheReadInputTokens?: number; cacheCreationInputTokens?: number; cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number }; stopReason?: string }
              turnTokens = usageEvt.totalTokens
              turnInputTokens = usageEvt.inputTokens ?? 0
              turnOutputTokens = usageEvt.outputTokens ?? 0
              // P0-C: capture the prompt-cache split for the tool-gating hit-rate baseline.
              turnCacheReadTokens = usageEvt.cacheReadInputTokens ?? 0
              turnCacheCreationTokens = usageEvt.cacheCreationInputTokens ?? 0
              // I1: per-slot attribution forwarded into TurnMetrics. Undefined when the provider
              // doesn't honor cache_control (OpenAI-family auto-cache).
              turnCacheReadBySlot = usageEvt.cacheReadInputTokensBySlot
              // Phase 4: stop_reason drives the kernel's max-output-tokens recovery. The closing
              // usage frame carries it; keep the last non-empty value seen this turn.
              if (usageEvt.stopReason) turnStopReason = usageEvt.stopReason
              continue
            }
            yield evt
            if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
            else if (evt.type === "tool_call") {
              const tc = evt as ToolCallEvent
              finalToolCalls.push({ id: tc.id, name: tc.name, arguments: JSON.stringify(tc.arguments) })
            }
          }
        } catch (err) {
          if (abortSignal?.aborted) {
            // External I/O is already stopped; the post-stream branch commits cancellation.
            this.interrupted = true
            this.cancellationReason ??= "user"
          } else {
            // Reactive recovery is now a kernel decision. Forward the raw provider error and
            // dispatch whatever the kernel returns: `call_provider` to retry with a freshly
            // compacted context, or `done` to terminate with an honest `ContextOverflow`. The
            // classify + compact + retry + give-up policy lives in the kernel (one place), not
            // duplicated across the four SDK runners. `continue` re-enters the loop: a recovered
            // turn persists its compaction archive via the loop-top appendObservations, and a
            // terminal `done` exits through `isTerminal()` into the run_terminal emit below.
            action = await this.commitKernelAction(runtime, this.pendingObservations, {
              kind: "provider_error",
              effect_id: providerEffectId,
              message: formatToolError(err),
            })
            // Withholding (query.ts parity): surface the raw provider error only when the kernel
            // could NOT recover (it returned a terminal). On a recovered retry (`call_provider`)
            // the error stays hidden, so embedders that terminate on `error` events don't see a
            // phantom failure mid-recovery.
            if (action.kind === "done") {
              yield { type: "error", message: formatToolError(err) } as ErrorEvent
            }
            continue
          }
        }

        // Do not commit partial provider output after host cancellation.
        if (abortSignal?.aborted) {
          action = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "cancel_operation",
            reason: this.cancellationReason ?? "user",
            pending_call_ids: [providerEffectId],
          })
          break
        }

        const assistantMessage: Message = {
          role: "assistant",
          content: finalText,
          toolCalls: finalToolCalls,
          tokenCount: turnOutputTokens || turnTokens || undefined,
        }
        const providerEvent: Record<string, unknown> = {
          kind: "provider_result",
          effect_id: providerEffectId,
          message: messageToKernelMessage(assistantMessage),
          ...(turnInputTokens > 0 ? { observed_input_tokens: turnInputTokens } : {}),
          ...(turnOutputTokens > 0 ? { observed_output_tokens: turnOutputTokens } : {}),
          now_ms: Date.now(),
          ...(turnStopReason ? { stop_reason: turnStopReason } : {}),
        }
        action = await this.commitKernelAction(runtime, this.pendingObservations, providerEvent)
        const providerReplay = peekProviderReplay(this.opts.provider, finalText, finalToolCalls)
        await this.opts.sessionLog.append(sessionId, buildLlmCompletedEvent({
          turn: runtime.turn(),
          content: finalText,
          tokenCount: turnOutputTokens || turnTokens || undefined,
          toolCalls: finalToolCalls,
          providerReplay,
        }))

        // P0-C: emit per-turn tool-gating telemetry. `activeSkill` reflects the skill in effect
        // GOING INTO this turn; a `skill` call here only takes effect next turn, so emit first, then
        // advance. Wrapped so a faulty sink can never break the run (pure observation).
        if (this.opts.onTurnMetrics) {
          try {
            this.opts.onTurnMetrics({
              turn: runtime.turn(),
              toolsExposed: tools.length,
              toolsCalled: finalToolCalls.length,
              activeSkill,
              inputTokens: turnInputTokens,
              cacheReadTokens: turnCacheReadTokens,
              cacheCreationTokens: turnCacheCreationTokens,
              ...(turnCacheReadBySlot ? { cacheReadTokensBySlot: turnCacheReadBySlot } : {}),
            })
          } catch { /* metrics must never break the run */ }
        }
        const skillCall = finalToolCalls.find(c => c.name === "skill")
        if (skillCall) {
          try {
            const name = (JSON.parse(skillCall.arguments || "{}") as { name?: string }).name
            if (name) activeSkill = name
          } catch { /* malformed skill args — leave activeSkill unchanged */ }
        }

      } else if (action.kind === "request_approval") {
        const resolved = await this.resolveApprovalRequests(action.requests, runtime, sessionId)
        for (const event of resolved.events) yield event
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "approval_result",
          effect_id: action.effectId,
          approved_calls: resolved.approved,
          denied_calls: resolved.denied,
        })

      } else if (action.kind === "persist_memory") {
        let error: string | undefined
        const agentId = this.opts.agentId
        try {
          if (!agentId) throw new Error("memory persistence requires RuntimeOptions.agentId")
          await this.persistMemoryToStore(action.memory as unknown as MemoryRecord, agentId)
        } catch (cause) {
          error = formatToolError(cause)
        }
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "memory_persist_result",
          effect_id: action.effectId,
          ...(error ? { error } : {}),
        })

      } else if (action.kind === "query_memory") {
        const query = action.query as unknown as MemoryQuery
        let hits: MemoryRecall[] = []
        let error: string | undefined
        const agentId = this.opts.agentId
        try {
          if (!agentId) throw new Error("memory queries require RuntimeOptions.agentId")
          hits = await this.retrieveMemoryFromStore(query, action.requestedK, agentId)
        } catch (cause) {
          error = formatToolError(cause)
        }
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "memory_query_result",
          effect_id: action.effectId,
          hits,
          ...(error ? { error } : {}),
        })
        if (!error) await this.logMemoryRetrievalResult(sessionId, hits)

      } else if (action.kind === "spool_large_result") {
        const spool = this.opts.resultSpool ?? new LargeResultSpool()
        let spoolRef: string | undefined
        let error: string | undefined
        try {
          spoolRef = await spool.persistOutput(sessionId, action.callId, action.output)
        } catch (cause) {
          error = formatToolError(cause)
        }
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "large_result_spool_result",
          effect_id: action.effectId,
          ...(spoolRef ? { spool_ref: spoolRef } : {}),
          ...(error ? { error } : {}),
        })

      } else if (action.kind === "archive_page_out") {
        const archiveMeta: { archiveStart: number; compressedSeq: number } = this.activePageOutArchive
          ?? this.pendingPageOutArchives.shift()
          ?? {
            archiveStart: this.nextArchiveStart,
            compressedSeq: await this.opts.sessionLog.latestSeq(sessionId),
          }
        this.activePageOutArchive = archiveMeta

        let archiveRef: string | undefined
        let error: string | undefined
        try {
          if (this.opts.compressionStore) {
            const ref = await this.opts.compressionStore.write(
              sessionId,
              archiveMeta.archiveStart,
              action.archived,
            )
            if (ref) archiveRef = ref
          }
        } catch (cause) {
          error = formatToolError(cause)
        }

        const archived = action.archived
        const archiveAction = compressionAction(action.action) ?? "auto_compact"
        const archiveTier = action.tier
        const compressedSeq = archiveMeta.compressedSeq
        if (!error) this.activePageOutArchive = undefined
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "page_out_archive_result",
          effect_id: action.effectId,
          ...(archiveRef ? { archive_ref: archiveRef } : {}),
          ...(error ? { error } : {}),
        })

        if (!error) {
          if (this.opts.asyncSummarizer && archived.length > 0) {
            const upgrade = () => this.upgradeCompressedSummary(
              sessionId,
              compressedSeq,
              archived,
              archiveAction,
            )
            taskScope.spawn("compressed-summary-upgrade", upgrade)
          }
          if (archiveTier === "semantic" && archived.length > 0) {
            taskScope.spawn("semantic-page-out", () => this.archiveSemanticPageOut(
              archived,
              archiveAction,
              sessionId,
            ))
          }
        }

      } else if (action.kind === "execute_tool") {
        const toolEffectId = action.effectId
        const allCalls: ToolCall[] = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        const runCtx: RunContext = {
          operation,
          agentId: this.opts.agentId,
          memoryScope: this.opts.memoryScope,
          skillDir: this.opts.skillDir,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          onToolSuspend: this.opts.onToolSuspend,
          onPermissionRequest: this.opts.onPermissionRequest,
          resultSpool: this.opts.resultSpool ?? new LargeResultSpool(),
        }

        const toolResults: ToolResult[] = []
        const normalCalls = allCalls.filter(
          c => c.name !== "update_plan" && c.name !== "submit_workflow_nodes" && c.name !== "start_workflow"
            && c.name !== "read_result",
        )
        const planCalls = allCalls.filter(c => c.name === "update_plan")
        // M5 v1: `start_workflow` (author a sub-workflow) flattens to the same append path as
        // `submit_workflow_nodes` — a `WorkflowSpec` is a node batch. (v2 adds top-level bootstrap.)
        const submitCalls = allCalls.filter(c => c.name === "submit_workflow_nodes" || c.name === "start_workflow")
        // O7: `read_result` re-fetches a tool output the kernel evicted from context. Content is
        // host-resolved from the effect-committed spool, then from the durable session log.
        const readResultCalls = allCalls.filter(c => c.name === "read_result")

        for (const call of planCalls) {
          const update = parseUpdatePlanArgs(call.arguments)
          await this.commitKernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel(update),
          })
          const result = { callId: call.id, output: "success", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: "success", isError: false } as ToolResultEvent
        }

        for (const call of readResultCalls) {
          const out = await this.resolveReadResult(sessionId, call.arguments)
          toolResults.push({ callId: call.id, output: out.text, isError: out.isError })
          yield { type: "tool_result", callId: call.id, content: out.text, isError: out.isError } as ToolResultEvent
        }

        // R3-1: `submit_workflow_nodes` cannot be applied to this runner's kernel — when this runner
        // is a workflow node, the workflow lives in the *parent* kernel. Surface the requested nodes
        // as a stream event; the orchestrator collects them onto the node's result and `runWorkflow`
        // sends `submit_workflow_nodes` to the parent kernel. (When not a workflow node, the event is
        // simply unconsumed — a no-op.)
        for (const call of submitCalls) {
          // M5 v2.1: a TOP-LEVEL agent authoring a whole sub-workflow via `start_workflow` — record the
          // full spec and AUTO-PIVOT once this tool turn resolves (the loop drives it in this kernel and
          // injects the outcome). A workflow-NODE's `start_workflow` (and every `submit_workflow_nodes`)
          // instead FLATTENS: the batch is surfaced for the parent `runWorkflow` to append.
          if (call.name === "start_workflow" && !this.opts.isWorkflowNode) {
            const spec = parseStartWorkflowSpec(call.arguments)
            if (spec) {
              this.pendingAuthoredWorkflows.push(spec)
              const out = "workflow submitted for governance adjudication"
              toolResults.push({ callId: call.id, output: out, isError: false })
              yield { type: "tool_result", callId: call.id, content: out, isError: false } as ToolResultEvent
              continue
            }
          }
          // `start_workflow` wraps the batch as `{ spec: { nodes } }`; `submit_workflow_nodes` is `{ nodes }`.
          const nodes = call.name === "start_workflow"
            ? parseStartWorkflowArgs(call.arguments)
            : parseSubmitWorkflowNodesArgs(call.arguments)
          yield { type: "workflow_nodes_submitted", nodes } as WorkflowNodesSubmittedEvent
          const result = { callId: call.id, output: "workflow nodes submitted for parent governance adjudication", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: result.output, isError: false } as ToolResultEvent
        }

        // O5 (PreToolUse-hook analog): give the host a STATEFUL veto over each kernel-approved
        // call. A blocked call never executes; its reason reaches the model as a committed
        // governance-denied tool result. Decision failures are closed
        // unless the host explicitly marks this hook advisory with `onToolCallFailure: "open"`.
        let executableCalls = normalCalls
        if (this.opts.onToolCall) {
          const allowed: ToolCall[] = []
          for (const call of normalCalls) {
            let decision: ToolCallHookDecision | undefined | void
            try {
              decision = await this.opts.onToolCall({ callId: call.id, name: call.name, arguments: call.arguments })
            } catch (cause) {
              decision = this.opts.onToolCallFailure === "open"
                ? undefined
                : { block: true, reason: `onToolCall hook failed: ${formatToolError(cause)}` }
            }
            if (decision?.block) {
              const reason = decision.reason ?? "blocked by host onToolCall hook"
              yield { type: "tool_denied", callId: call.id, toolName: call.name, reason } as ToolDeniedEvent
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_denied", turn: runtime.turn(), call_id: call.id, tool_name: call.name, reason,
              })
              const out = `blocked by host hook: ${reason}`
              toolResults.push({ callId: call.id, output: out, isError: true, errorKind: "governance_denied" })
              yield { type: "tool_result", callId: call.id, name: call.name, content: out, isError: true } as ToolResultEvent
              continue
            }
            allowed.push(call)
          }
          executableCalls = allowed
        }

        if (executableCalls.length > 0) {
          for await (const evt of this.opts.executionPlane.executeAll(executableCalls, runCtx)) {
            yield evt
            if (evt.type === "tool_result") {
              const tre = evt as ToolResultEvent
              toolResults.push({
                callId: tre.callId,
                output: tre.content,
                isError: tre.isError,
                isFatal: tre.isFatal,
                errorKind: tre.errorKind,
              })
            } else if (evt.type === "tool_argument_repaired") {
              const tare = evt as ToolArgumentRepairedEvent
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_argument_repaired",
                turn: runtime.turn(),
                tool: tare.name,
                original_arguments: tare.originalArguments,
                repaired_arguments: tare.repairedArguments,
              })
            } else if (evt.type === "tool_denied") {
              const tde = evt as ToolDeniedEvent
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_denied",
                turn: runtime.turn(),
                call_id: tde.callId,
                tool_name: tde.toolName,
                reason: tde.reason,
              })
            } else if (evt.type === "permission_request") {
              const pre = evt as PermissionRequestEvent
              const turn = runtime.turn()
              await this.opts.sessionLog.append(sessionId, {
                kind: "permission_requested",
                turn,
                tool: pre.toolName,
                arguments: pre.arguments,
                reason: pre.reason,
              })
            } else if (evt.type === "permission_resolved") {
              const resolved = evt as PermissionResolvedEvent
              const turn = runtime.turn()
              await this.opts.sessionLog.append(sessionId, {
                kind: "permission_resolved",
                turn,
                approved: resolved.approved,
                responder: resolved.responder,
              })
            }
          }
          const names = executableCalls.map(c => c.name).join(", ")
          await this.commitKernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel({ progress: `Executed tools: ${names}` }),
          })
        }

        // O5 (PostToolUse-hook analog): let the host inspect each executed result BEFORE it
        // reaches the kernel/session-log — replace the output (redact/annotate) and/or push a
        // contextual note into the signal stream. Errs-open on hook throw.
        if (this.opts.onToolResult) {
          for (const r of toolResults) {
            const call = executableCalls.find(c => c.id === r.callId)
            if (!call) continue // plan/submit synthetics and hook-blocked calls are not host results
            let decision: ToolResultHookDecision | undefined | void
            try {
              decision = await this.opts.onToolResult({
                callId: r.callId, name: call.name, arguments: call.arguments,
                output: r.output, isError: r.isError,
              })
            } catch { decision = undefined }
            if (!decision) continue
            if (typeof decision.replaceOutput === "string") r.output = decision.replaceOutput
            if (decision.note) this.injectNote(decision.note)
          }
        }

        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: toolResults.map(r => ({
            call_id: r.callId,
            output: r.output,
            is_error: r.isError,
            token_count: r.tokenCount,
          })),
        })
        // P1-B B3: a `skill` call that resolved successfully activates that skill in the kernel, so
        // the next `call_provider` narrows the toolset to its declared tools. Fed before `tool_results`
        // (which computes the next action). Errs-open: a failed/missing skill load doesn't activate.
        //
        // Strict dynamic context control: a skill is METHOD content — how to do something — reused
        // for the rest of the run, unlike a one-off memory/knowledge lookup (fact content, relevant
        // for the moment it's used). So its text ALSO goes into the durable `knowledge` slot here
        // (in addition to the ordinary tool_result already headed for `history`, where it will decay
        // with the compression pyramid like any other tool output — that's fine, the permanent copy
        // now lives in `knowledge`). First activation only (see `knowledgePushedSkills`).
        for (const call of allCalls) {
          if (call.name !== "skill") continue
          const res = toolResults.find(r => r.callId === call.id)
          if (!res || res.isError) continue
          try {
            const name = (JSON.parse(call.arguments || "{}") as { name?: string }).name
            if (!name) continue
            await this.commitKernelApply(runtime, this.pendingObservations, {
              kind: "skill_activated",
              name,
              ...(this.opts.skillLeaseTurns !== undefined ? { lease_turns: this.opts.skillLeaseTurns } : {}),
            })
            // K1: keyed `skill:<name>` — the kernel-side upsert dedupes across runner instances
            // (wake re-push of an already-pinned skill upserts instead of duplicating). With a
            // lease configured, the Set optimization is skipped: an expired-then-reloaded skill
            // must re-pin, and only the kernel knows the lease state — its upsert dedupes anyway.
            if (this.opts.skillLeaseTurns !== undefined || !this.knowledgePushedSkills.has(name)) {
              this.knowledgePushedSkills.add(name)
              await this.pushKnowledge(
                { role: "system", content: res.output, toolCalls: [] },
                undefined,
                { key: `skill:${name}` },
              )
            }
          } catch { /* malformed skill args — skip activation */ }
        }
        const entropyObsStart = this.pendingObservations.length
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "tool_results",
          effect_id: toolEffectId,
          results: toolResults.map(toolResultToKernel),
        })
        // Surface the boundary's entropy measurement live (the heartbeat watch source) —
        // the session-log record lands via the normal appendObservations path.
        for (const obs of this.pendingObservations.slice(entropyObsStart)) {
          if (obs.kind === "entropy_sample") {
            this.lastEntropySample = entropySampleFromObservation(obs)
            yield { type: "entropy_sample", sample: this.lastEntropySample } as EntropySampleEvent
          } else if (obs.kind === "entropy_alert") {
            yield {
              type: "entropy_alert",
              turn: obs.turn ?? 0,
              score: obs.score ?? 0,
              threshold: obs.threshold ?? 0,
            } as EntropyAlertEvent
          }
        }

      } else if (action.kind === "evaluate_milestone") {
        const milestoneEffectId = action.effectId
        const milestonePolicy = this.opts.milestonePolicy ?? "require_verifier"
        if (milestonePolicy === "auto_pass") {
          action = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: milestoneEffectId,
            result: milestoneCheckResultToKernel(milestoneCheckPass(action.phaseId)),
          })
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
            taskScope,
          )
        } else if (this.opts.onMilestoneEvaluate) {
          const check = await this.opts.onMilestoneEvaluate({
            phaseId: action.phaseId,
            criteria: action.criteria,
            requiredEvidence: action.requiredEvidence,
          })
          action = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: milestoneEffectId,
            result: milestoneCheckResultToKernel(check),
          })
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
            taskScope,
          )
        } else {
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
            taskScope,
          )
          const turnsUsed = Math.max(1, runtime.turn())
          await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
            reason: "milestone_pending",
            turnsUsed,
            totalTokens: 0,
          }))
          await groupBudgetScope?.release()
          await taskScope.drain()
          yield { type: "done", iterations: turnsUsed, totalTokens: 0, status: "milestone_pending" } as DoneEvent
          this.activeKernel = null
          this.currentSessionId = null
          return
        }

      } else if (action.kind === "done") {
        break
      }
    }
    } catch (err) {
      // I0b: kernel rejection (or any other thrown error inside the loop) reaches us here.
      // Classify by NAPI status code or message pattern — `invalid_arg` for surface-shape rejects,
      // `error` for everything else — then emit run_terminal so observability sees a clean end.
      // The yield-error path mirrors what the in-flight provider-stream catch does.
      const errMsg = formatToolError(err)
      const code = (err as { code?: string }).code
      const isInvalidArg = code === "InvalidArg" ||
        errMsg.toLowerCase().includes("invalidarg") ||
        errMsg.toLowerCase().includes("invalid argument")
      const reason = isInvalidArg ? "invalid_arg" : "error"
      yield { type: "error", message: errMsg } as ErrorEvent
      try {
        await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
          reason,
          turnsUsed: runtime.turn() || 0,
          totalTokens: 0,
        }))
      } catch { /* session log failure must not mask the original error */ }
      await groupBudgetScope?.release()
      await taskScope.drain()
      yield { type: "done", iterations: runtime.turn() || 0, totalTokens: 0, status: reason } as DoneEvent
      this.activeKernel = null
      this.currentSessionId = null
      this.dashboard = null
      return
    }

    const result = action.kind === "done" ? action.result : undefined
    // I0a: when the loop exits without a clean kernel-done — typically because a hard interrupt
    // aborted the in-flight LLM stream and the catch path sent `timeout` (which the kernel handles
    // by injecting a rollback note and continuing, not by terminating) — preserve the preempt
    // intent in the run_terminal reason. Without this, every interrupt-curtailed run reports
    // `reason: "error"` and the bench / observability layer can't distinguish preemption from a
    // genuine crash. Mirrors WASM/Python/Rust.
    const status = result?.termination ?? "error"
    const turnsUsed = result ? Math.max(1, result.turnsUsed) : runtime.turn() || 0
    const totalTokens = result?.totalTokensUsed ?? 0

    nextCompressedArchiveStart = await this.appendObservations(
      sessionId,
      runtime,
      nextCompressedArchiveStart,
      taskScope,
    )
    await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
      reason: status,
      turnsUsed,
      totalTokens,
    }))

    if (groupBudgetScope && !groupBudgetScope.isClosed) {
      throw new Error("kernel terminated without a correlated budget_usage_reported observation")
    }

    if (this.opts.dreamStore && this.opts.agentId) {
      const newMsgs = runtime.drainNewMessages().map(m => ({
        role: m.role,
        content: m.content,
        contentParts: m.contentParts,
        tokenCount: m.tokenCount,
        toolCalls: m.toolCalls?.length ? m.toolCalls : undefined,
      }))
      if (newMsgs.length > 0) {
        try {
          const completedSession: SessionData = {
            sessionId,
            agentId: this.opts.agentId,
            messages: newMsgs,
            metadata: null,
            createdAtMs: sessionStart,
            updatedAtMs: Date.now(),
          }
          await this.opts.dreamStore.saveSession(completedSession)
          if (this.opts.memoryScope) {
            const extracted = await extractSessionMemories(
              this.opts.dreamProvider ?? this.opts.provider,
              completedSession,
              this.opts.memoryScope,
              this.opts.dreamSystemPrompt,
            )
            for (const memory of extracted) {
              await this.writeMemory(memory, { sessionId, agentId: this.opts.agentId })
            }
          }
        } catch { /* non-fatal */ }
      }
    }

    await taskScope.drain()
    yield {
      type: "done",
      iterations: turnsUsed,
      totalTokens,
      status,
      // ③ loop-agent: surface the kernel-adjudicated after-round decision to the driver.
      ...(result?.paceDecision ? { paceDecision: result.paceDecision } : {}),
    } as DoneEvent
    this.activeKernel = null
    this.currentSessionId = null
    this.dashboard = null
    } finally {
      await groupBudgetScope?.release()
      if (taskScope.pending > 0) await taskScope.cancel("run scope closed")
      this.activeKernel = null
      this.currentSessionId = null
      this.dashboard = null
    }
  }

  /** I4 + K4: fetch long-term memory hits for the current goal and land them in `history` as an
   *  ordinary user turn — single-use retrieval content that decays with the compression pyramid,
   *  never pinned into `knowledge`. Called once before turn 1 (`phase: "initial"`) and re-fired
   *  after each sprint renewal (`phase: "renewal"`): renewal drops the old history INCLUDING the
   *  earlier memory hits, so the new sprint gets a fresh recall pass. Errs-open throughout. */
  private async prefetchMemoryIntoHistory(
    runtime: KernelRuntimeInstance,
    phase: "initial" | "renewal",
  ): Promise<KernelRunnerAction | undefined> {
    if (!this.opts.dreamStore || !this.opts.agentId || !this.opts.memoryScope) return undefined
    // P10: recall is default-on (CC session-start recall) — with no hook configured,
    // the goal itself is the query. preQueryMemory stays as the targeting override.
    const preQuery = this.opts.preQueryMemory
      ?? ((ctx: { goal: string }) => [{
        scope: this.opts.memoryScope!,
        query: ctx.goal,
        top_k: 5,
        kinds: [],
      }])
    try {
      const queries = await preQuery({
        goal: this.currentGoal,
        runSpec: this.opts.runSpec,
        phase,
      })
      // T5: route each query through the kernel's query_memory lifecycle instead of calling
      // the store directly — the kernel injects each routed hit into history itself and the
      // recall lifecycle (recordRecall / promotion) fires exactly like an in-run query.
      //
      // One prefetch = one dedupe horizon: a record hit by several short queries recalls and
      // injects once. A renewal prefetch starts a fresh horizon — renewal dropped the earlier
      // injection with the old history, so re-exposure is a genuine new recall.
      const seenRecordIds = new Set<string>()
      let resumed: KernelRunnerAction | undefined
      for (const q of queries ?? []) {
        if (!q.query.trim()) continue
        const { action } = await this.queryMemoryThroughKernel(
          runtime,
          q,
          this.opts.agentId,
          this.durableSessionId(this.currentSessionId),
          seenRecordIds,
          this.pendingObservations,
        )
        resumed = action ?? resumed
      }
      // Each memory_query_result resumes the reasoning path and re-emits the pending loop
      // action; the caller must continue from the LAST one (it renders the injected hits).
      return resumed
    } catch { /* errs-open — a faulty pre-fetch never breaks the run */ }
    return undefined
  }

  private async appendObservations(
    sessionId: string,
    runtime: KernelRuntimeInstance,
    nextArchiveStart: number,
    _taskScope?: ManagedTaskScope,
  ): Promise<number> {
    const turn = runtime.turn()
    const preservedRefs = runtime.preservedRefs()
    const observations = this.pendingObservations.splice(0)
    for (const obs of observations) {
      if (obs.kind === "page_in_requested") continue
      if (obs.kind === "budget_usage_reported") {
        const scope = this.activeGroupBudgetScope
        if (!scope || obs.reservation_id !== scope.reservationId) {
          throw new Error("budget usage report does not match the active reservation")
        }
        await this.settleGroupBudget(scope, {
          tokens: obs.tokens ?? 0,
          subagents: obs.subagents ?? 0,
          rounds: obs.rounds ?? 0,
        })
        this.activeGroupBudgetScope = undefined
      }

      // M3/M4: mirror the kernel's journaled recall lifecycle into the durable store /
      // host callbacks (shared consumer — identical semantics on every query route).
      await this.mirrorMemoryLifecycle(obs)

      const latest =
        obs.kind === "compressed" ? await this.opts.sessionLog.latestSeq(sessionId) : undefined
      const event = kernelObservationToSessionEvent(obs, turn, {
        nextArchiveStart,
        latestSeq: latest,
        preservedRefs,
        compressionAction,
      })
      if (!event) continue

      const compressedSeq = await this.opts.sessionLog.append(sessionId, event)
      if (event.kind === "compressed") {
        if ((obs.archived_count ?? 0) > 0) {
          this.pendingPageOutArchives.push({ archiveStart: nextArchiveStart, compressedSeq })
        }
        nextArchiveStart = compressedSeq + 1
      }
      // K4: a sprint renewal dropped the old history — including any earlier memory hits — so
      // re-run the preQueryMemory prefetch for the new sprint (live observations only: this
      // consumer sits on the live drain path, same placement as the semantic page-out archival).
      if (obs.kind === "renewed") {
        await this.prefetchMemoryIntoHistory(runtime, "renewal")
      }
    }
    return nextArchiveStart
  }

  private async archiveSemanticPageOut(
    archived: Message[],
    action: string | undefined,
    sessionId: string,
  ): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId || !this.opts.memoryScope) return
    const summary = this.opts.dreamSummarizer
      ? await this.opts.dreamSummarizer.summarize(archived, { action })
      : await summarizeForLongTermMemory(
        this.opts.dreamProvider ?? this.opts.provider,
        archived,
        this.opts.dreamSystemPrompt,
      )
    // P2 write-funnel: route through the ONE gated WriteMemory syscall so validation,
    // the rolling write quota, dedup, and the memory_written audit all apply. Score is
    // advisory (0.6) — an automatic summary must never outrank curated content.
    const now = Date.now()
    const name = `page-out-${now}`
    await this.writeMemory({
      record_id: `${this.opts.memoryScope.tenant_id}:${this.opts.memoryScope.namespace}:project:${name}`,
      scope: this.opts.memoryScope,
      name,
      kind: "project",
      content: summary,
      description: `auto summary of ${action ?? "compaction"} archive`,
      provenance: {
        session_id: sessionId,
        author: "extraction",
        trust: "untrusted",
        evidence_refs: [],
      },
      created_at: now,
      updated_at: now,
      recall_count: 0,
      confidence: 0.6,
      links: [],
      pinned: false,
    }, { sessionId, agentId: this.opts.agentId })
  }

  private async upgradeCompressedSummary(
    sessionId: string,
    compressedSeq: number,
    archived: Message[],
    action: string,
    runtime?: KernelRuntimeInstance,
  ): Promise<void> {
    const summary = await this.opts.asyncSummarizer!.summarize(archived, action)
    await this.opts.sessionLog.append(sessionId, {
      kind: "summary_upgraded",
      compressed_seq: compressedSeq,
      summary,
    })
    // P4: the LLM summary also re-enters the LIVE session as a keyed page-in entry —
    // K1 boundary-deferred upsert lands it with zero mid-generation cache churn and the
    // K2 budget governs its size. The RuleSummarizer text remains the synchronous label.
    if (runtime) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "page_in",
        entries: [{
          content: `[ARCHIVE SUMMARY] ${summary}`,
          key: `summary:seq-${compressedSeq}`,
          pinned: false,
          source: "async_summarizer",
        }],
      })
    }
  }
}

function isMidRun(events: Array<{ seq: number; event: SessionEvent }>): boolean {
  // Mid-run ⇔ the LAST run_started has no run_terminal after it. Pairing (not mere
  // presence) matters on multi-round loop sessions: round 1's terminal must not make a
  // crashed round 2 look fresh, and driver-level round_* records must not make a fresh
  // round look interrupted.
  let lastStarted = -1
  let lastTerminal = -1
  for (let i = 0; i < events.length; i++) {
    const k = events[i].event.kind
    if (k === "run_started") lastStarted = i
    else if (k === "run_terminal") lastTerminal = i
  }
  return lastStarted >= 0 && lastStarted > lastTerminal
}

/**
 * True when an earlier run in this session already seeded the same attachments. Replay
 * reconstructs the attachment message from that run's `run_started`, so recording and
 * live-seeding them again (a same-session retry attempt) would double them in history.
 */
function attachmentsAlreadySeeded(
  prior: Array<{ seq: number; event: SessionEvent }>,
  attachments: ContentPart[],
): boolean {
  const wanted = JSON.stringify(attachments)
  return prior.some(({ event }) =>
    event.kind === "run_started" && JSON.stringify(event.attachments ?? []) === wanted)
}

/**
 * Build a kernel `add_history_message` payload from user attachments: a `user`
 * message whose content is the multimodal parts in the kernel's serde shape
 * (`Content::Parts`; image `media_type`, not `mediaType`). Lets a caller upload
 * images/audio with the task — the message lands in history before the first render.
 */
function attachmentsToKernelMessage(parts: ContentPart[]): Record<string, unknown> {
  const content = parts.map(p => {
    if (p.type === "image") {
      return {
        type: "image",
        ...(p.url ? { url: p.url } : {}),
        ...(p.data ? { data: p.data } : {}),
        ...(p.mediaType ? { media_type: p.mediaType } : {}),
        ...(p.detail ? { detail: p.detail } : {}),
      }
    }
    if (p.type === "audio") return { type: "audio", data: p.data, media_type: p.mediaType }
    if (p.type === "text") return { type: "text", text: p.text }
    return { type: "text", text: "" }
  })
  return { role: "user", content }
}

function compressionAction(action?: string): Extract<SessionEvent, { kind: "compressed" }>["action"] {
  if (
    action === "snip_compact" ||
    action === "micro_compact" ||
    action === "context_collapse" ||
    action === "auto_compact"
  ) {
    return action
  }
  return undefined
}

async function summarizeForLongTermMemory(
  provider: LLMProvider,
  archived: Message[],
  systemPrompt?: string,
): Promise<string> {
  const transcript = archived
    .map(m => `${m.role}: ${m.content}`)
    .join("\n")
  const context = {
    systemText: [
      systemPrompt,
      "Summarize the following conversation for long-term memory. Preserve key facts, decisions, and open questions.",
    ].filter(Boolean).join("\n\n"),
    turns: [{ role: "user" as const, content: transcript, toolCalls: [] }],
  }
  let text = ""
  const state = provider.createRunState?.()
  for await (const evt of provider.stream(context, [], undefined, state)) {
    if (evt.type === "text_delta" && "delta" in evt) text += evt.delta
  }
  return text.trim() || transcript.slice(0, 2000)
}

/** Kernel-consumed meta-tools (e.g. `pace`) are answered by a synthetic tool result the kernel keeps
 *  in its OWN history but never emits as a `tool_completed` session event (they never reach the
 *  execution plane). On replay that leaves an assistant `tool_call` with no following tool result —
 *  which strict OpenAI-compatible providers reject ("every tool_call must be answered by a tool
 *  message"). This pass re-pairs any such orphan by inserting a synthetic tool-result message right
 *  after its assistant message, reproducing the pair the kernel had all along.
 *
 *  Discriminator: only pair an orphan when the run **continued past it** — i.e. a later non-tool
 *  message exists. A tail assistant tool_call with nothing after it is a genuinely PENDING tool the
 *  run stopped in front of (the wake/recovery case), which must stay unpaired so wake executes it.
 *  Pure. */
export function pairOrphanToolCalls(messages: Message[]): Message[] {
  const out: Message[] = []
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i]
    out.push(m)
    if (m.role !== "assistant" || !m.toolCalls?.length) continue
    // Collect ids answered by the immediately-following run of tool messages; note where it ends.
    const answered = new Set<string>()
    let j = i + 1
    for (; j < messages.length && messages[j].role === "tool"; j++) {
      for (const p of messages[j].contentParts ?? []) {
        if (p.type === "tool_result") answered.add(p.callId)
      }
    }
    // If nothing follows the tool run, this tool_call is a pending tail (wake case) — leave it.
    if (j >= messages.length) continue
    for (const c of m.toolCalls) {
      if (answered.has(c.id)) continue
      out.push({
        role: "tool",
        content: "",
        toolCalls: [],
        contentParts: [{ type: "tool_result", callId: c.id, output: `[${c.name} handled by kernel]`, isError: false }],
        tokenCount: 1,
      })
    }
  }
  return out
}

export function replayMessages(events: Array<{ seq: number; event: SessionEvent }>, maxBytes?: number): Message[] {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
  for (let eventIndex = 0; eventIndex < events.length; eventIndex++) {
    const { seq, event: e } = events[eventIndex]!
    if (e.kind === "run_started") {
      const userText = e.criteria.length
        ? `${e.goal}\n\nCriteria:\n${e.criteria.map((c, i) => `${i + 1}. ${c}`).join("\n")}`
        : e.goal
      // Multimodal parity: the live seed of `attachments` is gated behind `!resumeMidRun`, so on
      // resume the image/audio must be recovered from the persisted run_started event or it is lost.
      const attachments = ((e as { attachments?: ContentPart[] }).attachments ?? [])
      const contentParts: ContentPart[] | undefined = attachments.length
        ? [...(userText ? [{ type: "text", text: userText } as ContentPart] : []), ...attachments]
        : undefined
      messages.push({
        role: "user",
        content: userText,
        ...(contentParts ? { contentParts } : {}),
        toolCalls: [],
        tokenCount: Math.max(1, Math.ceil(userText.length / 4)),
      })
    } else if (e.kind === "compressed") {
      const summary = upgradedSummaries.get(seq) ?? e.summary
      if (summary) {
        const systemText = `[Compressed context: turn ${e.turn}]\n${summary}`
        messages.push({
          role: "system",
          content: systemText,
          toolCalls: [],
          tokenCount: Math.max(1, Math.ceil(systemText.length / 4)),
        })
      }
    } else if (e.kind === "llm_completed") {
      messages.push({
        role: "assistant",
        content: sanitizeReplayText(e.content, maxBytes),
        toolCalls: e.tool_calls ?? [],
        tokenCount: e.token_count,
      })
    } else if (e.kind === "tool_completed") {
      for (const r of e.results) {
        messages.push({
          role: "tool",
          content: "",
          toolCalls: [],
          contentParts: [{ type: "tool_result", callId: r.call_id, output: sanitizeReplayText(r.output, maxBytes), isError: r.is_error ?? false }],
          tokenCount: r.token_count,
        })
      }
    } else if (e.kind === "rollbacked") {
      const len = e.checkpoint_history_len ?? 0
      if (messages.length > len) {
        messages.length = len
      }
    }
  }
  return pairOrphanToolCalls(messages)
}

export async function replayMessagesAsync(
  events: Array<{ seq: number; event: SessionEvent }>,
  maxBytes?: number,
  loadArchive?: (archiveRef: string) => Promise<Message[]>,
): Promise<Message[]> {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
  for (let eventIndex = 0; eventIndex < events.length; eventIndex++) {
    const { seq, event: e } = events[eventIndex]!
    if (e.kind === "run_started") {
      const userText = e.criteria.length
        ? `${e.goal}\n\nCriteria:\n${e.criteria.map((c, i) => `${i + 1}. ${c}`).join("\n")}`
        : e.goal
      // Multimodal parity: the live seed of `attachments` is gated behind `!resumeMidRun`, so on
      // resume the image/audio must be recovered from the persisted run_started event or it is lost.
      const attachments = ((e as { attachments?: ContentPart[] }).attachments ?? [])
      const contentParts: ContentPart[] | undefined = attachments.length
        ? [...(userText ? [{ type: "text", text: userText } as ContentPart] : []), ...attachments]
        : undefined
      messages.push({
        role: "user",
        content: userText,
        ...(contentParts ? { contentParts } : {}),
        toolCalls: [],
        tokenCount: Math.max(1, Math.ceil(userText.length / 4)),
      })
    } else if (e.kind === "compressed") {
      const pageOutWillSupplyArchive = events.slice(eventIndex + 1).some(({ event }) =>
        event.kind === "page_out"
          && event.turn === e.turn
          && typeof event.archive_ref === "string"
          && event.archive_ref.length > 0,
      )
      if (!pageOutWillSupplyArchive) {
        const summary = upgradedSummaries.get(seq) ?? e.summary
        if (summary) {
          const systemText = `[Compressed context: turn ${e.turn}]\n${summary}`
          messages.push({
            role: "system",
            content: systemText,
            toolCalls: [],
            tokenCount: Math.max(1, Math.ceil(systemText.length / 4)),
          })
        }
      }
    } else if (e.kind === "page_out" && e.archive_ref && loadArchive) {
      try {
          const archivedMsgs = await loadArchive(e.archive_ref)
          for (const msg of archivedMsgs) {
            messages.push({
              role: msg.role,
              content: sanitizeReplayText(msg.content, maxBytes),
              toolCalls: msg.toolCalls ?? [],
              tokenCount: msg.tokenCount,
            })
          }
      } catch {
          if (e.summary) {
            const systemText = `[Compressed context: turn ${e.turn}]\n${e.summary}`
            messages.push({
              role: "system",
              content: systemText,
              toolCalls: [],
              tokenCount: Math.max(1, Math.ceil(systemText.length / 4)),
            })
          }
      }
    } else if (e.kind === "llm_completed") {
      messages.push({
        role: "assistant",
        content: sanitizeReplayText(e.content, maxBytes),
        toolCalls: e.tool_calls ?? [],
        tokenCount: e.token_count,
      })
    } else if (e.kind === "tool_completed") {
      for (const r of e.results) {
        messages.push({
          role: "tool",
          content: "",
          toolCalls: [],
          contentParts: [{ type: "tool_result", callId: r.call_id, output: sanitizeReplayText(r.output, maxBytes), isError: r.is_error ?? false }],
          tokenCount: r.token_count,
        })
      }
    } else if (e.kind === "rollbacked") {
      const len = e.checkpoint_history_len ?? 0
      if (messages.length > len) {
        messages.length = len
      }
    }
  }
  return pairOrphanToolCalls(messages)
}

function nextArchivedSeqStart(events?: Array<{ seq: number; event: SessionEvent }>): number {
  let next = 0
  for (const { event } of events ?? []) {
    if (event.kind === "compressed") next = Math.max(next, event.archived_seq_range[1] + 1)
  }
  return next
}

/** Collect all text_delta events from a run into a single string. */
export async function collectText(stream: AsyncIterable<StreamEvent>): Promise<string> {
  let text = ""
  for await (const evt of stream) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  return text
}

function parseUpdatePlanArgs(argsStr: string): import("../types.js").TaskUpdate {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error
  }
  return {
    plan: parsed.plan as string[] | undefined,
    currentStep: parsed.currentStep !== undefined ? Number(parsed.currentStep) : parsed.current_step !== undefined ? Number(parsed.current_step) : undefined,
    progress: parsed.progress as string | undefined,
    scratchpad: parsed.scratchpad as string | undefined,
    blockedOn: parsed.blockedOn !== undefined
      ? parsed.blockedOn as string[]
      : parsed.blocked_on as string[] | undefined,
  }
}

/** R3-1: parse the `submit_workflow_nodes` tool arguments (`{ nodes: WorkflowNodeSpec[] }`). Node
 *  shapes are trusted structurally here; the kernel validates them (dep range, quarantine, quota) on
 *  append. A malformed payload yields no nodes rather than throwing. */
function parseSubmitWorkflowNodesArgs(argsStr: string): WorkflowNodeSpec[] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → no nodes submitted.
  }
  return Array.isArray(parsed.nodes) ? (parsed.nodes as WorkflowNodeSpec[]) : []
}

/** M5 v1: parse the `start_workflow` tool arguments (`{ spec: { nodes: WorkflowNodeSpec[] } }`) into
 *  the spec's node batch — flattened onto the running workflow via the same append path. A malformed
 *  payload yields no nodes rather than throwing. */
function parseStartWorkflowArgs(argsStr: string): WorkflowNodeSpec[] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → no nodes.
  }
  const spec = parsed.spec as { nodes?: unknown } | undefined
  return Array.isArray(spec?.nodes) ? (spec!.nodes as WorkflowNodeSpec[]) : []
}

/** M5 v2.1: parse the full `WorkflowSpec` from a top-level `start_workflow` call, for auto-pivot drive
 *  (vs `parseStartWorkflowArgs`, which returns only the node batch for the flatten path). Returns
 *  `undefined` on a malformed / empty payload so the caller falls back to the flatten path. */
function parseStartWorkflowSpec(argsStr: string): WorkflowSpec | undefined {
  try {
    const parsed = JSON.parse(argsStr) as { spec?: { nodes?: unknown } }
    if (Array.isArray(parsed.spec?.nodes) && parsed.spec!.nodes.length > 0) {
      return { nodes: parsed.spec!.nodes as WorkflowNodeSpec[] }
    }
  } catch {
    // Ignore parse error → undefined (fall back to flatten).
  }
  return undefined
}

/** M5 v2.1: render an authored-workflow outcome into a user-message note injected back into the
 *  agent's context, so the agent's next turn continues with the sub-workflow's results in view. */
function recoveredOutputs(outcomes: RecoveredNodeOutcome[] | undefined): Map<string, string> {
  const outputs = new Map<string, string>()
  for (const outcome of outcomes ?? []) {
    if (!outcome.output) continue
    outputs.set(outcome.agentId, outcome.output.content)
    outputs.set(outcome.agentId.replace(/-i\d+$/, ""), outcome.output.content)
  }
  return outputs
}

function authoredWorkflowOutcomeNote(outcome: WorkflowOutcome): string {
  const counts = new Map<string, number>()
  for (const node of outcome.nodeOutcomes) counts.set(node.status, (counts.get(node.status) ?? 0) + 1)
  const lines = [
    `[authored workflow result] ${outcome.nodeOutcomes.length} terminal node(s): ` +
      [...counts.entries()].map(([status, count]) => `${count} ${status}`).join(", ") + ".",
  ]
  for (const node of outcome.nodeOutcomes) {
    const out = outcome.outputs[node.nodeId] ?? node.output?.content
    if (out) lines.push(`- ${node.nodeId} (${node.status}): ${out.length > 500 ? out.slice(0, 500) + "…" : out}`)
  }
  return lines.join("\n")
}

/** Lower a host `RuntimeSignal` to the kernel's snake_case `signal` input event. Shared by the main
 *  loop's per-turn poll and #2-B-ii's workflow-batch preemption monitor (so the two never drift). */
function signalToKernelEvent(delivery: InboundSignalDelivery): Record<string, unknown> {
  const sig = delivery.signal
  return {
    kind: "deliver_signal",
    delivery_id: delivery.deliveryId,
    attempt: delivery.deliveryAttempt,
    signal: {
      id: delivery.signalId,
      source: sig.source ?? "custom",
      signal_type: sig.signalType ?? "event",
      urgency: sig.urgency ?? "normal",
      summary: String((sig.payload as Record<string, unknown>)?.goal ?? "signal"),
      payload: sig.payload ?? {},
      ...(sig.dedupeKey ? { dedupe_key: sig.dedupeKey } : {}),
      ...(sig.recipient ? { recipient: sig.recipient } : {}),
      ...(sig.deadlineMs !== undefined ? { deadline_ms: sig.deadlineMs } : {}),
      ...(sig.coalesceKey ? { coalesce_key: sig.coalesceKey } : {}),
      coalesced_count: Math.max(1, sig.coalescedCount ?? 1),
      timestamp_ms: Date.now(),
    },
  }
}
