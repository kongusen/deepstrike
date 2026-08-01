import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema, ContentPart,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions,
  DreamSummarizer,
} from "../types.js"
import type { ToolSuspendEvent } from "./execution-plane.js"
import type { DreamStore, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope, SessionData } from "../memory/index.js"
import { extractSessionMemories } from "../memory/extraction.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import type { SignalSource, RuntimeSignal, SignalDeliveryReceipt } from "../signals/index.js"
import type { SessionLog, SessionEvent } from "./session-log.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { governancePolicyToKernelEvent, governanceFilterSchema, type GovernancePolicy } from "../governance.js"
import { getKernel } from "./kernel.js"
import {
  CanonicalRunnerRuntime,
  canonicalKernelAction,
  canonicalKernelApply,
  canonicalKernelMaybeAction,
  canonicalStartAgent,
  canonicalStartWorkflow,
  sha256 as canonicalSha256,
} from "./canonical-kernel-step.js"
import type { KernelJournal } from "./kernel-journal.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import { formatToolError } from "../tools/errors.js"
import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  buildWorkflowNodeCompletedEvent,
  buildWorkflowNodesSubmittedEvent,
} from "./session-repair.js"
import {
  messageToKernelMessage,
  skillMetadataToKernel,
  taskUpdateToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
  type KernelObservation,
  type KernelRunnerAction,
} from "./kernel-step.js"
import type { AgentRunSpec, AgentProcessChangedObservation, SubAgentResult, MilestonePolicy, MilestoneContract, MilestoneCheckResult, WorkflowSpec, WorkflowSpawnInfo, WorkflowBudget, WorkflowOutcome, WorkflowNodeOutcome, KernelWorkflowNodeOutcome } from "./types/agent.js"
import {
  agentRunSpecToKernel,
  MILESTONE_UNVERIFIED_REASON,
  milestoneCheckFail,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  subAgentResultToKernel,
  workflowBudgetNote,
  workflowNodeSpecToKernel,
  workflowNodeOutcomeFromKernel,
  workflowNodeStatusFromTermination,
  workflowNodeToManifest,
  workflowNodeToSpec,
  workflowSpecToKernel,
} from "./types/agent.js"
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
import { kernelObservationToSessionEvent } from "./kernel-event-log.js"
import { assertNativeProfile, type NativeOsProfile, type OsProfileId, type SignalPolicy } from "./os-profile.js"
import { PayloadStore } from "./payload-store.js"
import {
  contextPolicyV1,
  normalizeContextPolicyV1,
  type ContextPolicyOverridesV1,
} from "./context-policy.js"

export interface MemoryWriteRateLimit {
  maxWrites: number
  windowMs: number
}

export interface ResourceQuota {
  /** Max sub-agents in the `running` state at once; further spawns are denied while at cap. */
  maxConcurrentSubagents?: number
  /** Max sub-agent nesting depth (direct children of the root loop are depth 1). */
  maxSpawnDepth?: number
  /** Max nodes in one in-kernel workflow DAG, including dynamically submitted nodes. */
  maxWorkflowNodes?: number
  /** Rolling-window memory-write rate limit: at most `maxWrites` per any `windowMs` span. */
  memoryWritesPerWindow?: MemoryWriteRateLimit
}

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

/**
 * Long-term memory policy (`set_memory_policy`) — opt-in, kernel-enforced. `validationEnabled:
 * false` admits writes without validation, `maxContentBytes` / `maxNameLength` override the
 * validation limits, and `retrievalTopK` caps `query_memory` breadth. Host storage is configured
 * on the `DreamStore` and never enters this contract. Omitted fields keep the kernel defaults.
 */
export interface MemoryPolicy {
  staleWarningDays?: number
  retrievalTopK?: number
  validationEnabled?: boolean
  maxContentBytes?: number
  maxNameLength?: number
}

export interface KernelReliabilityOptions {
  providerRecoveryAttempts?: number
  outputRecoveryAttempts?: number
  /** Max canonical JSON bytes accepted for one kernel input, 256..64MiB. */
  maxInputBytes?: number
}

function kernelReliabilityToKernel(policy: KernelReliabilityOptions): Record<string, number> {
  const allowed = new Set(["providerRecoveryAttempts", "outputRecoveryAttempts", "maxInputBytes"])
  const unknown = Object.keys(policy).filter(key => !allowed.has(key))
  if (unknown.length > 0) {
    throw new TypeError(`unknown kernel reliability field(s): ${unknown.join(", ")}`)
  }
  return {
    ...(policy.providerRecoveryAttempts !== undefined
      ? { provider_recovery_attempts: policy.providerRecoveryAttempts }
      : {}),
    ...(policy.outputRecoveryAttempts !== undefined
      ? { output_recovery_attempts: policy.outputRecoveryAttempts }
      : {}),
    ...(policy.maxInputBytes !== undefined ? { max_input_bytes: policy.maxInputBytes } : {}),
  }
}

function memoryPolicyToKernel(policy: MemoryPolicy): Record<string, unknown> {
  const allowed = new Set([
    "staleWarningDays",
    "retrievalTopK",
    "validationEnabled",
    "maxContentBytes",
    "maxNameLength",
  ])
  const unknown = Object.keys(policy).filter(key => !allowed.has(key))
  if (unknown.length > 0) {
    throw new TypeError(`unknown memory policy field(s): ${unknown.join(", ")}`)
  }
  return {
    ...(policy.staleWarningDays !== undefined ? { stale_warning_days: policy.staleWarningDays } : {}),
    ...(policy.retrievalTopK !== undefined ? { retrieval_top_k: policy.retrievalTopK } : {}),
    ...(policy.validationEnabled !== undefined ? { validation_enabled: policy.validationEnabled } : {}),
    ...(policy.maxContentBytes !== undefined ? { max_content_bytes: policy.maxContentBytes } : {}),
    ...(policy.maxNameLength !== undefined ? { max_name_length: policy.maxNameLength } : {}),
  }
}

interface InboundSignalDelivery {
  signalId: string
  deliveryId: string
  deliveryAttempt: number
  signal: RuntimeSignal
  ack(): Promise<boolean>
  nack(): Promise<boolean>
}

export interface ArchiveStore {
  write(sessionId: string, startSeq: number, messages: Message[]): Promise<string | undefined>
  read?(archiveRef: string): Promise<Message[]>
}

/** P0-C tool-gating telemetry: per-LLM-turn metrics, emitted via `RuntimeOptions.onTurnMetrics`.
 *  Pure observation — no behavior change. `toolsExposed` vs `toolsCalled` quantifies over-exposure;
 *  consecutive equal `activeSkill` values measure skill dwell `D`; the cache split gives the
 *  prompt-cache hit baseline. Mirrors the node SDK. */
export interface TurnMetrics {
  turn: number
  toolsExposed: number
  toolsCalled: number
  activeSkill?: string
  inputTokens: number
  cacheReadTokens: number
  /** I1: pro-rata per-slot attribution of `cacheReadTokens` (Anthropic only). Mirrors Node. */
  cacheReadTokensBySlot?: { system?: number; tools?: number; messages?: number }
  cacheCreationTokens: number
}

/** O5: decision returned by `onToolCall` — `block: true` denies this call before it executes. */
export interface ToolCallHookDecision {
  block?: boolean
  reason?: string
}

/** O5: decision returned by `onToolResult` — replace the output and/or inject a signal note. */
export interface ToolResultHookDecision {
  replaceOutput?: string
  note?: string
}

export interface RuntimeOptions {
  provider: LLMProvider
  /** Required for ABI-v3 operation recovery unless the SessionLog embeds one. */
  kernelJournal?: KernelJournal
  /** M1/G3 intelligence routing: resolve a per-node provider from a workflow node's `modelHint`.
   *  Returns undefined ⇒ fall back to `provider`. Without this hook the hint is a no-op. */
  providerFor?: (modelHint: string) => LLMProvider | undefined
  /** M4/G5: cumulative token cap for this run (the kernel's `max_total_tokens`); a node's `tokenBudget`
   *  flows here for its child run. Undefined ⇒ the kernel default. */
  maxTotalTokens?: number
  sessionLog: SessionLog
  compressionStore?: ArchiveStore
  executionPlane: ExecutionPlane
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  agentId?: string
  memoryScope?: MemoryScope
  /** I4: optional run-start memory pre-fetch hook (mirrors Node SDK). Called once per run before
   *  the first LLM turn; each returned query string becomes a dreamStore search; hits page into
   *  the knowledge partition before turn 1. Requires dreamStore + agentId. */
  preQueryMemory?: (ctx: {
    goal: string
    /** K4: `"initial"` = pre-turn-1 fetch; `"renewal"` = re-fired after a sprint renewal. */
    phase?: "initial" | "renewal"
  }) => Promise<MemoryQuery[] | undefined> | MemoryQuery[] | undefined
  systemPrompt?: string
  initialMemory?: string[]
  /** Skill name → markdown body (WASM has no filesystem). */
  skillContentMap?: Map<string, string>
  dreamStore?: DreamStore
  /** M4: advisory callback when a recalled record crosses the promotion threshold. */
  onPromotionSuggested?: (info: { recordId: string; recallCount: number }) => void
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  /** Named or concrete OS profile. Defaults to the native microkernel profile. */
  osProfile?: OsProfileId | NativeOsProfile
  governancePolicy?: GovernancePolicy
  signalPolicy?: SignalPolicy
  promptBudget?: PromptBudget
  /** Stable replayable context behavior; ratios are normalized to integer ppm. */
  contextPolicy?: ContextPolicyOverridesV1
  schedulerPolicy?: SchedulerPolicy
  kernelReliability?: KernelReliabilityOptions
  /** Attempts allowed for a workflow node to satisfy its output schema, 1..16. Default: 2. */
  workflowSchemaValidationAttempts?: number
  resourceQuota?: ResourceQuota
  /** O6: the in-kernel repeat fuse — identical tool call (same name AND args) `denyAfter` turns in a
   *  row ⇒ deny + directive note; `terminateAfter` ⇒ run ends `no_progress`. Defaults 5/8; `false`
   *  disables. Same-tool/different-args loops never trip it. */
  repeatFuse?: { denyAfter?: number; terminateAfter?: number } | false
  /** O4: turn-end criteria gate — one kernel-injected self-check turn before accepting completion
   *  while `criteria` stand. Default enabled; `false` accepts the first finish unconditionally. */
  criteriaGate?: boolean
  /** K2: max share of `maxTokens` the durable knowledge partition may occupy. Over budget ⇒
   *  warn-once observation + oldest unpinned non-skill entries evicted at the next boundary.
   *  Pinned/skill entries are exempt. `0` disables. Default: kernel's 0.25. */
  knowledgeBudgetRatio?: number
  /** Opt-in kernel entropy watch: threshold alerting over the per-turn session-entropy score
   *  (`entropy_sample` events stream unconditionally regardless). See the Node SDK's
   *  `entropyWatch` for the canonical documentation. Absent ⇒ disabled (kernel default). */
  entropyWatch?: EntropyWatchOptions
  /** K3: default lease (in turns) for every skill activation — auto-deactivates after N turns
   *  (toolset re-widens, knowledge pin boundary-swept). Absent ⇒ permanent (default). */
  skillLeaseTurns?: number
  /** O5 (PreToolUse-hook analog): stateful host veto over each kernel-approved call; return
   *  `{ block: true, reason }` to deny — the reason reaches the model as a denied result. Errs-open. */
  onToolCall?: (call: { callId: string; name: string; arguments: string }) =>
    Promise<ToolCallHookDecision | undefined | void> | ToolCallHookDecision | undefined | void
  /** O5 (PostToolUse-hook analog): inspect each executed result; `{ replaceOutput }` swaps what the
   *  model sees, `{ note }` injects a signal note (the `injectNote` channel). Errs-open. */
  onToolResult?: (result: { callId: string; name: string; arguments: string; output: string; isError: boolean }) =>
    Promise<ToolResultHookDecision | undefined | void> | ToolResultHookDecision | undefined | void
  memoryPolicy?: MemoryPolicy
  tokenizer?: string
  enablePlanTool?: boolean
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  subAgentOrchestrator?: SubAgentOrchestrator
  /** G2: custom reducers for `NodeKind::Reduce` workflow nodes, merged over the built-ins. */
  reducers?: ReducerRegistry
  milestonePolicy?: MilestonePolicy
  milestoneContract?: MilestoneContract
  onMilestoneEvaluate?: (ctx: { phaseId: string; criteria: string[]; requiredEvidence: string[] }) => Promise<MilestoneCheckResult> | MilestoneCheckResult
  runSpec?: AgentRunSpec
  /**
   * The run's **exposure ceiling** — the outer bound on what this run may EVER advertise to the
   * model. Not a static profile: it is an INTERSECTION applied on every turn (`exposed ⊆ ceiling`),
   * so every narrowing mechanism operates *within* it and none can widen past it. Skills narrow
   * inside the ceiling (`allowed_tools`), `baselineToolIds` selects which of the ceiling's tools are
   * exposed before any skill activates, and `stableCoreToolIds` pins tools against skill narrowing.
   *
   * Exempt on the id axis: the kernel-owned meta-tools (`skill`, `memory`, `knowledge`,
   * `update_plan`, `read_result`) stay exposed regardless of this list — a ceiling that hid `skill`
   * would make progressive disclosure unreachable. The KIND axis
   * (`runSpec.capabilityFilter.allowedKinds`) still applies to them.
   *
   * Byte-stable across the run, so it never busts the prompt-cache prefix. Lowers to the same
   * `capability_filter` sub-agents use: augments `runSpec`'s filter when both are set, else
   * synthesizes a minimal run spec. Omitted **or empty** ⇒ no ceiling (all registered tools) — the
   * empty array is NOT a minimal surface here; use `baselineToolIds: []` for that.
   *
   * Enforcement: `toolDispatchGate` (default `"exposed"`) makes this a real boundary — a call to a
   * tool outside the advertised set never executes.
   */
  allowedToolIds?: string[]
  /**
   * The **pre-activation** exposure surface, selected from under the `allowedToolIds` ceiling.
   * Makes the narrow→wide progressive-disclosure shape expressible: start the run advertising only
   * these tools, and let a skill activation widen the surface by exactly its declared
   * `allowed_tools` (still ∩ the ceiling). Per turn:
   *
   *   `exposed = meta ∪ ((baseline ∪ stableCore ∪ ⋃ activeSkills.allowed_tools) ∩ ceiling)`
   *
   * An active skill that declares no `allowed_tools` contributes nothing — with a baseline set the
   * surface stays narrow (strict; the legacy errs-open widening is deliberately not inherited).
   *
   * `undefined` ⇒ legacy behavior, byte-identical. `[]` is a legitimate, distinct value: the minimal
   * surface (meta-tools + `stableCoreToolIds` only) — the `allowedToolIds` "empty means no gating"
   * trap does NOT recur here. Entries outside the ceiling silently intersect away.
   */
  baselineToolIds?: string[]
  /**
   * Dispatch enforcement for the exposure surface. `"exposed"` (default) is fail-closed: a tool call
   * the model was never advertised this turn never reaches the host — the kernel commits a
   * model-visible `governance_denied` result instead, which feeds the repeat fuse like any other
   * denial. Allowed siblings in the same batch still execute; `pace` and the meta-tool family always
   * pass through. `"registered"` is the escape hatch restoring the pre-gate permissive behavior (any
   * registered tool the model names executes, even if it was gated out of the tools schema).
   */
  toolDispatchGate?: "exposed" | "registered"
  /** P0-C: optional per-turn metrics sink for tool-gating telemetry (see `TurnMetrics`). Pure
   *  observation; invoked once per LLM turn. Never throws into the run loop (errors are swallowed). */
  onTurnMetrics?: (metrics: TurnMetrics) => void
  /** P1-B/D stable-core: tool ids always exposed under skill gating. Empty/absent ⇒ skills narrow
   *  to exactly their declared tools + meta-tools. (wasm skills come from `skillContentMap`; gating
   *  engages only once that carries per-skill tool lists.) */
  stableCoreToolIds?: string[]
  dreamProvider?: LLMProvider
  dreamSummarizer?: DreamSummarizer
  dreamSystemPrompt?: string
  payloadStore?: PayloadStore
}

export type OperationCancellationReason = "user" | "deadline" | "lease_lost" | "host_shutdown"

function pendingCallIds(action: KernelRunnerAction): string[] {
  switch (action.kind) {
    case "call_provider": return [action.effectId]
    case "execute_tool": return action.calls.map(call => call.id)
    case "request_approval": return action.requests.map(request => request.callId)
    case "spawn_workflow": return action.nodes.map(node => String(node.agent_id ?? "")).filter(Boolean)
    case "preempt_sub_agents": return action.agentIds
    default: return "effectId" in action ? [action.effectId] : []
  }
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

export class RuntimeRunner {
  private interrupted = false
  private cancellationReason: OperationCancellationReason | undefined
  /** #2-B-ii: aborts the in-flight provider stream on interrupt/preempt. Recreated per `execute`. */
  private abortController: AbortController | null = null
  private pendingObservations: KernelObservation[] = []
  private activeKernel: CanonicalRunnerRuntime | null = null
  private currentSessionId: string | null = null
  private fallbackPayloadStore: PayloadStore | null = null
  /** O2 (system-reminder channel): host-pushed notes awaiting the next turn-boundary drain. */
  private injectedSignals: RuntimeSignal[] = []
  /** Skill names whose content has already been pushed into the durable `knowledge` slot this
   *  run — guards against re-pushing a duplicate entry if the model calls `skill(name)` again for
   *  an already-active skill (loading is idempotent; the knowledge push should be too). */
  private knowledgePushedSkills = new Set<string>()
  /** Most recent kernel entropy sample of the active/last run (see `latestEntropy`). */
  private lastEntropySample: EntropySample | null = null
  /** K4: the active run's goal, kept for the renewal-boundary memory re-query. */
  private currentGoal = ""
  private nextArchiveStart = 0
  private pendingPageOutArchives: Array<{ archiveStart: number; compressedSeq: number }> = []
  private activePageOutArchive: { archiveStart: number; compressedSeq: number } | undefined
  /** Provider continuation emitted after a canonical nested workflow completes. */
  private workflowContinuation: Extract<KernelRunnerAction, { kind: "call_provider" }> | null = null

  constructor(private readonly opts: RuntimeOptions) {
    const schemaAttempts = opts.workflowSchemaValidationAttempts ?? 2
    if (!Number.isInteger(schemaAttempts) || schemaAttempts < 1 || schemaAttempts > 16) {
      throw new RangeError("workflowSchemaValidationAttempts must be an integer between 1 and 16")
    }
  }

  private resolveKernelJournal(): KernelJournal {
    const embedded = this.opts.sessionLog as SessionLog & { kernelJournal?: KernelJournal }
    const journal = this.opts.kernelJournal ?? embedded.kernelJournal
    if (!journal) {
      throw new Error("RuntimeOptions.kernelJournal is required when SessionLog has no canonical journal")
    }
    return journal
  }

  private async createCanonicalRuntime(
    runId: string = crypto.randomUUID(),
    sessionId = this.currentSessionId ?? "wasm-session",
  ): Promise<CanonicalRunnerRuntime> {
    const { CanonicalKernel } = await getKernel()
    return new CanonicalRunnerRuntime(
      new CanonicalKernel(),
      this.resolveKernelJournal(),
      `wasm-operation-${runId}`,
      {
        maxContextTokens: this.opts.maxTokens,
        maxTurns: this.opts.maxTurns,
        maxTotalTokens: this.opts.maxTotalTokens,
        maxWallMs: this.opts.timeoutMs,
        memoryBindingId: `wasm-memory-${this.opts.agentId ?? "root"}`,
        persistPayload: async (_callId, content, previewBytes) => {
          const digest = canonicalSha256(content)
          const payloadRef = `payload:${digest.replace(/^sha256:/, "").slice(0, 32)}`
          await this.payloadStore().persistPayload(sessionId, payloadRef, content)
          return {
            payloadRef,
            digest,
            originalSize: String(new TextEncoder().encode(content).byteLength),
            preview: new TextDecoder().decode(new TextEncoder().encode(content).slice(0, previewBytes)),
          }
        },
      },
    )
  }

  private async commitKernelApply(
    runtime: CanonicalRunnerRuntime,
    pending: KernelObservation[],
    event: Record<string, unknown>,
  ): Promise<KernelObservation[]> {
    return canonicalKernelApply(runtime, pending, event) as Promise<KernelObservation[]>
  }

  private async commitKernelMaybeAction(
    runtime: CanonicalRunnerRuntime,
    pending: KernelObservation[],
    event: Record<string, unknown>,
  ): Promise<KernelRunnerAction | null> {
    return canonicalKernelMaybeAction(runtime, pending, event)
  }

  private async commitKernelAction(
    runtime: CanonicalRunnerRuntime,
    pending: KernelObservation[],
    event: Record<string, unknown>,
  ): Promise<KernelRunnerAction> {
    return canonicalKernelAction(runtime, pending, event)
  }

  get hostOptions(): RuntimeOptions { return this.opts }

  interrupt(reason: OperationCancellationReason = "user"): void {
    this.interrupted = true
    this.cancellationReason = reason
    this.abortController?.abort(reason)
  }

  /** Push a contextual note into the run's signal stream (the system-reminder channel): it drains at
   *  the next turn boundary, routes through the kernel attention policy, and renders once as a
   *  `[SIGNAL] <text>` line in the volatile state turn. `urgency` maps to
   *  the kernel disposition ladder: `"normal"` queues (default), `"high"` soft-interrupts, `"critical"`
   *  preempts. */
  injectNote(text: string, urgency: RuntimeSignal["urgency"] = "normal"): void {
    this.injectedSignals.push({ source: "custom", signalType: "event", urgency, payload: { goal: text } })
  }

  /** The most recent kernel session-entropy sample (one per completed turn), or `null` before the
   *  first boundary. A pull companion to the streamed `entropy_sample` events. */
  latestEntropy(): EntropySample | null {
    return this.lastEntropySample
  }

  /** Injected-note drain shared with the main loop's per-turn poll: injected notes first (FIFO), then
   *  the configured `signalSource` — one code path so the two inbound channels never drift. */
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
    const claim = await source.claimSignal()
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
    consume: (delivery: InboundSignalDelivery) => T | Promise<T>,
  ): Promise<Awaited<T>> {
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
      return result as Awaited<T>
    } catch (cause) {
      await delivery.nack()
      throw cause
    }
  }

  async *run(req: {
    sessionId: string
    goal: string
    criteria?: string[]
    extensions?: Record<string, unknown>
    attachments?: import("../types.js").ContentPart[]
    inheritEvents?: Array<{ seq: number; event: SessionEvent }>
  }): AsyncIterable<StreamEvent> {
    const prior = req.inheritEvents ?? await this.opts.sessionLog.read(req.sessionId)
    const resumedStart = [...prior].reverse().find(entry => entry.event.kind === "run_started")
    // Inherited parent events are transcript input for a fresh child operation, never recovery
    // evidence for the child's own canonical journal.
    let midRun = req.inheritEvents ? false : isMidRun(prior)
    if (!midRun && resumedStart?.event.kind === "run_started" && !req.inheritEvents) {
      const operationId = `wasm-operation-${resumedStart.event.run_id}`
      if (await this.resolveKernelJournal().head(operationId)) {
        const authoritative = await this.createCanonicalRuntime(resumedStart.event.run_id, req.sessionId)
        await authoritative.restore()
        if (!authoritative.isTerminal()) midRun = true
      }
    }
    // Idempotent per session: an earlier run's `run_started` already carries these attachments
    // (same-session retry attempt), so replay reconstructs them — recording and seeding again
    // would double them in history. Deduping at the append keeps live and replay in agreement.
    const attachments = req.attachments?.length && !attachmentsAlreadySeeded(prior, req.attachments)
      ? req.attachments
      : undefined
    const runId = midRun && resumedStart?.event.kind === "run_started"
      ? resumedStart.event.run_id
      : crypto.randomUUID()
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
      runId,
      req.goal,
      req.criteria ?? [],
      req.extensions,
      prior.length > 0 ? prior : undefined,
      midRun,
      attachments,
    )
  }

  async *wake(sessionId: string, extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const events = await this.opts.sessionLog.read(sessionId)
    const startIndex = events.reduce(
      (latest, entry, index) => entry.event.kind === "run_started" ? index : latest,
      -1,
    )
    const startEntry = startIndex >= 0 ? events[startIndex] : undefined
    if (!startEntry) throw new Error(`No run_started event for session: ${sessionId}`)
    const start = startEntry.event as Extract<SessionEvent, { kind: "run_started" }>
    const journalHead = await this.resolveKernelJournal().head(`wasm-operation-${start.run_id}`)
    if (!journalHead) {
      const projectedTail = events.slice(startIndex + 1)
      if (projectedTail.some(e => e.event.kind === "run_terminal")) {
        throw new Error("run_terminal projection has no canonical journal")
      }
      if (projectedTail.some(entry => entry.event.kind === "tool_requested" || entry.event.kind === "tool_completed")) {
        throw new Error("restored canonical operation has no pending effect or terminal")
      }
    } else {
      const authoritative = await this.createCanonicalRuntime(start.run_id, sessionId)
      await authoritative.restore()
      if (authoritative.isTerminal()) return
    }

    yield* this.execute(sessionId, start.run_id, start.goal, start.criteria, extensions, events, true, start.attachments)
  }

  async writeMemory(memory: MemoryRecord, sessionId?: string): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId) return
    try {
      await this.opts.dreamStore.upsert(this.opts.agentId, memory)
    } catch (cause) {
      throw new Error(formatToolError(cause))
    }
  }

  private async appendMemorySyscallObservations(sessionId: string | undefined, observations: KernelObservation[]): Promise<void> {
    if (!sessionId) return
    for (const observation of observations) {
      if (!["memory_written", "memory_queried", "memory_validation_failed"].includes(observation.kind)) continue
      const event = kernelObservationToSessionEvent(observation, 0)
      if (event) await this.opts.sessionLog.append(sessionId, event)
    }
  }

  /** Push content into Slot 2 (system_knowledge) via add_knowledge_message.
   *  K1: `opts.key` gives the entry identity — a same-key push upserts (applied at the next
   *  compaction/renewal boundary) instead of appending a duplicate. `opts.pinned` exempts the
   *  entry from the knowledge-budget sweep. */
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

  private payloadStore(): PayloadStore {
    if (this.opts.payloadStore) return this.opts.payloadStore
    this.fallbackPayloadStore ??= new PayloadStore()
    return this.fallbackPayloadStore
  }

  /** K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
   *  Errs-open: an unknown key is a kernel-side no-op. */
  async removeKnowledge(key: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, { kind: "remove_knowledge", key })
  }

  /** K3: host-driven skill deactivation — toolset re-widens at the next provider call, the
   *  skill's knowledge pin drops at the next boundary. Errs-open: not-active is a no-op. */
  async deactivateSkill(name: string): Promise<void> {
    if (!this.activeKernel) return
    await this.commitKernelApply(this.activeKernel, this.pendingObservations, { kind: "skill_deactivated", name })
    this.knowledgePushedSkills.delete(name)
  }

  private async resolveKernelSuspend(
    requests: Array<{ callId: string; tool: string; arguments: string; reason: string }>,
    runtime: CanonicalRunnerRuntime,
    sessionId: string,
  ): Promise<{ approved: string[]; denied: string[]; events: StreamEvent[] }> {
    const approved: string[] = []
    const denied: string[] = []
    const events: StreamEvent[] = []
    const runCtx: RunContext = { onPermissionRequest: this.opts.onPermissionRequest }

    for (const requestAction of requests) {
      const request: PermissionRequestEvent = {
        type: "permission_request",
        callId: requestAction.callId,
        toolName: requestAction.tool,
        arguments: requestAction.arguments,
        reason: requestAction.reason,
      }
      events.push(request)
      const decision = await resolvePermissionRequest(request, runCtx)
      events.push({
        type: "permission_resolved",
        callId: requestAction.callId,
        toolName: requestAction.tool,
        approved: decision.approved,
        responder: decision.responder ?? "host",
        ...(decision.reason ? { reason: decision.reason } : {}),
      } as PermissionResolvedEvent)
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_requested",
        turn: runtime.turn(),
        tool: requestAction.tool,
        arguments: requestAction.arguments,
        reason: request.reason,
      })
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_resolved",
        turn: runtime.turn(),
        approved: decision.approved,
        responder: decision.responder ?? "host",
      })
      if (decision.approved) {
        approved.push(requestAction.callId)
      } else {
        denied.push(requestAction.callId)
        const denyReason = decision.reason ?? "permission denied"
        events.push({
          type: "tool_denied",
          callId: requestAction.callId,
          toolName: requestAction.tool,
          reason: denyReason,
        } as ToolDeniedEvent)
        events.push({
          type: "tool_result",
          callId: requestAction.callId,
          name: requestAction.tool,
          content: `permission denied: ${denyReason}`,
          isError: true,
          errorKind: "governance_denied",
        } as ToolResultEvent)
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_denied",
          turn: runtime.turn(),
          call_id: requestAction.callId,
          tool_name: requestAction.tool,
          reason: denyReason,
        })
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: [{
            call_id: requestAction.callId,
            output: `permission denied: ${denyReason}`,
            is_error: true,
            error_kind: "governance_denied",
          }],
        })
      }
    }

    return { approved, denied, events }
  }

  private async *execute(
    sessionId: string,
    runId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
    attachments?: import("../types.js").ContentPart[],
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.cancellationReason = undefined
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId
    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    const providerPolicy = (this.opts.provider as { runtimePolicy?: () => { maxTurns?: number; timeoutMs?: number } }).runtimePolicy?.() ?? {}
    const effectiveMaxTurns = this.opts.maxTurns ?? providerPolicy.maxTurns ?? 25
    const effectiveTimeoutMs = this.opts.timeoutMs ?? providerPolicy.timeoutMs

    const runtime = await this.createCanonicalRuntime(runId, sessionId)
    if (resumeMidRun) await runtime.restore()
    this.activeKernel = runtime

    if (!resumeMidRun) {
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

    if (this.opts.skillContentMap && this.opts.skillContentMap.size > 0) {
      const metas = [...this.opts.skillContentMap.keys()].map(name => ({
        name,
        description: "",
        estimatedTokens: 0,
      }))
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_available_skills",
        skills: metas.map(skillMetadataToKernel),
      })
    }

    // P1-B/D: configure stable-core tool ids (always exposed under skill gating).
    if (this.opts.stableCoreToolIds?.length) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_stable_core_tools",
        tool_ids: this.opts.stableCoreToolIds,
      })
    }

    if (this.opts.dreamStore && this.opts.agentId) {
      await this.commitKernelApply(runtime, this.pendingObservations, { kind: "set_memory_enabled", enabled: true })
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
            verifier: p.verifier ?? null,
            required_evidence: p.requiredEvidence ?? [],
          })),
        },
      })
    }

    const maxBytes = runtime.recoveryContentBytes()
    if (priorEvents && priorEvents.length > 0) {
      const replayed = await replayMessages(priorEvents, maxBytes, this.opts.compressionStore)
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayed.map(messageToKernelMessage),
      })
      // P1-B B3: rebuild active-skill gating after a wake (active_skills is not snapshotted).
      // `knowledge` isn't snapshotted either (same graceful-reset philosophy) — best-effort re-push
      // the skill's content from its replayed tool_result so the durable copy survives a wake too.
      const toolResultByCallId = new Map<string, string>()
      for (const m of replayed) {
        for (const part of m.contentParts ?? []) {
          if (part.type === "tool_result" && part.callId && part.output !== undefined) {
            toolResultByCallId.set(part.callId, part.output)
          }
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
              // K1: keyed — the kernel-side upsert is the authoritative dedup across wake replays.
              this.pushKnowledge({ role: "system", content: output }, undefined, { key: `skill:${name}` })
            }
          } catch { /* skip */ }
        }
      }
    }

    // Multimodal upload: seed attachments before the canonical root start (parity with Node/Python).
    if (!resumeMidRun && attachments?.length) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: attachmentsToKernelMessage(attachments),
      })
    }
    }

    if (priorEvents && priorEvents.length > 0) {
      seedProviderReplayFromEvents(this.opts.provider, priorEvents)
    }

    const sessionStart = Date.now()
    const startTask: Record<string, unknown> = { goal, criteria }
    let startRunSpec: Record<string, unknown> | undefined
    // P0-A: lower an explicit `runSpec`, the `allowedToolIds` ceiling, and/or the `baselineToolIds`
    // pre-activation surface to the kernel run spec (reuses the existing run_spec wire — no new
    // ABI). Unset on all ⇒ no gating (铁律: no config = old behavior).
    const allowedToolIds = this.opts.allowedToolIds
    const hasProfile = allowedToolIds !== undefined && allowedToolIds.length > 0
    // NOT the `length > 0` idiom above: `baselineToolIds: []` is the legitimate minimal surface
    // (meta + stable-core only), so mere presence triggers the lowering.
    const baselineToolIds = this.opts.baselineToolIds
    const hasBaseline = baselineToolIds !== undefined
    if (this.opts.runSpec || hasProfile || hasBaseline) {
      const baseSpec: AgentRunSpec = this.opts.runSpec ?? {
        identity: { agentId: this.opts.agentId ?? "root", sessionId, isSubAgent: false },
        role: "custom",
        goal,
      }
      let spec: AgentRunSpec = hasProfile
        ? { ...baseSpec, capabilityFilter: { ...baseSpec.capabilityFilter, allowedIds: allowedToolIds } }
        : baseSpec
      if (hasBaseline) spec = { ...spec, exposureBaseline: baselineToolIds }
      startRunSpec = agentRunSpecToKernel(spec)
    }
    if (!resumeMidRun) await this.applyKernelPolicies(runtime)

    // I4: pre-fetch memory before the first LLM turn (mirrors Node). Strict dynamic context
    // control: single-use retrieval content, not a stable skill — lands in `history` like an
    // ordinary `memory` tool result, so it decays with the compression pyramid instead of
    // pinning itself in `knowledge` forever.
    this.currentGoal = goal
    if (!resumeMidRun) {
      await this.prefetchMemoryIntoHistory(runtime, "initial")
    }

    let action: KernelRunnerAction = resumeMidRun
      ? runtime.resumeAction() ?? (() => { throw new Error("restored canonical operation has no pending effect or terminal") })()
      : await canonicalStartAgent(runtime, this.pendingObservations, startTask, startRunSpec)
    // P0-C: the skill loaded and in effect going into the current turn → per-turn `activeSkill` metric.
    let activeSkill: string | undefined

    // I0b: kernel-throw safety net — see Node runner for full rationale.
    try {
    while (!runtime.isTerminal()) {
      nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
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
          const sigAction = await this.consumeInboundSignal(delivery, claimed =>
            this.commitKernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(claimed)))
          if (sigAction) action = sigAction
          // Critical attention/preemption is distinct from operation cancellation.
        }
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        const providerEffectId = action.effectId
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        // I5: governance schema-level pre-filter — see Node runner for full rationale.
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
            // #2-B-ii: a preempting interrupt fires abortController — stop consuming the live stream.
            if (abortSignal?.aborted) break
            if (evt.type === "usage") {
              const usageEvt = evt as { type: string; totalTokens: number; inputTokens?: number; outputTokens?: number; cacheReadInputTokens?: number; cacheCreationInputTokens?: number; cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number }; stopReason?: string }
              turnTokens = usageEvt.totalTokens
              turnInputTokens = usageEvt.inputTokens ?? 0
              turnOutputTokens = usageEvt.outputTokens ?? 0
              // P0-C: capture the prompt-cache split for the tool-gating hit-rate baseline.
              turnCacheReadTokens = usageEvt.cacheReadInputTokens ?? 0
              turnCacheCreationTokens = usageEvt.cacheCreationInputTokens ?? 0
              // I1: per-slot attribution forwarded to TurnMetrics; undefined on non-Anthropic providers.
              turnCacheReadBySlot = usageEvt.cacheReadInputTokensBySlot
              // Phase 4: stop_reason drives the kernel's max-output-tokens recovery.
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
            // #2-B-ii: an aborted in-flight request surfaces as an AbortError — treat it as an
            // interrupt (the post-stream `aborted` check below converts it to a clean
            // timeout/UserAbort), not a crash or a provider error.
            this.interrupted = true
            this.cancellationReason ??= "user"
          } else {
            // Reactive recovery is now a kernel decision. Forward the raw provider error and
            // dispatch whatever the kernel returns: `call_provider` to retry with a freshly
            // compacted context, or `done` to terminate with an honest `ContextOverflow`. The
            // classify + compact + retry + give-up policy lives in the kernel (one place), not
            // duplicated across the four SDK runners.
            action = await this.commitKernelAction(runtime, this.pendingObservations, {
              kind: "provider_error",
              effect_id: providerEffectId,
              message: formatToolError(err),
            })
            // Withholding (query.ts parity): surface the raw provider error only when the kernel
            // could NOT recover (it returned a terminal). On a recovered retry (`call_provider`)
            // the error stays hidden. `continue` re-enters the loop: a recovered turn persists its
            // compaction archive via the loop-top appendObservations, and a terminal `done` exits
            // through `isTerminal()`.
            if (action.kind === "done") {
              yield { type: "error", message: formatToolError(err) } as ErrorEvent
            }
            continue
          }
        }

        // #2-B-ii: stream aborted (preempt/interrupt) via the break path — end the turn now.
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

        // P0-C: per-turn tool-gating telemetry. `activeSkill` reflects the skill in effect GOING INTO
        // this turn; a `skill` call here only takes effect next turn — emit first, then advance.
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
        const resolved = await this.resolveKernelSuspend(action.requests, runtime, sessionId)
        for (const evt of resolved.events) yield evt
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "approval_result",
          effect_id: action.effectId,
          approved_calls: resolved.approved,
          denied_calls: resolved.denied,
        })

      } else if (action.kind === "persist_memory") {
        let error: string | undefined
        try {
          if (!this.opts.dreamStore || !this.opts.agentId) throw new Error("WASM memory persistence requires dreamStore and agentId")
          await this.opts.dreamStore.upsert(this.opts.agentId, action.memory as unknown as MemoryRecord)
        } catch (cause) { error = formatToolError(cause) }
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "memory_persist_result",
          effect_id: action.effectId,
          ...(error ? { error } : {}),
        })

      } else if (action.kind === "query_memory") {
        let hits: MemoryRecall[] = []
        let error: string | undefined
        try {
          if (!this.opts.dreamStore || !this.opts.agentId) throw new Error("WASM memory queries require dreamStore and agentId")
          hits = await this.opts.dreamStore.search(this.opts.agentId, {
            ...(action.query as unknown as MemoryQuery), top_k: action.requestedK,
          })
        } catch (cause) { error = formatToolError(cause) }
        const obsStart = this.pendingObservations.length
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "memory_query_result",
          effect_id: action.effectId,
          hits,
          ...(error ? { error } : {}),
        })
        // M3/M4: mirror the kernel's journaled recall lifecycle + surface promotion suggestions.
        for (const obs of this.pendingObservations.slice(obsStart)) {
          if (obs.kind === "memory_recalled" && obs.recalls?.length && this.opts.agentId) {
            await this.opts.dreamStore?.recordRecall?.(this.opts.agentId, obs.recalls)
          }
          if (obs.kind === "promotion_suggested" && obs.record_id) {
            this.opts.onPromotionSuggested?.({ recordId: obs.record_id, recallCount: obs.recall_count ?? 0 })
          }
        }

      } else if (action.kind === "archive_page_out") {
        const archiveMeta: { archiveStart: number; compressedSeq: number } = this.activePageOutArchive
          ?? this.pendingPageOutArchives.shift()
          ?? { archiveStart: this.nextArchiveStart, compressedSeq: await this.opts.sessionLog.latestSeq(sessionId) }
        this.activePageOutArchive = archiveMeta
        let archiveRef: string | undefined
        let error: string | undefined
        try {
          if (this.opts.compressionStore) {
            archiveRef = await this.opts.compressionStore.write(sessionId, archiveMeta.archiveStart, action.archived)
          }
          if (action.payload) {
            archiveRef ??= `payload:${action.payload.digest.replace(/^sha256:/, "").slice(0, 32)}`
            await this.payloadStore().persistPayload(sessionId, archiveRef, action.payload.content)
          }
        } catch (cause) {
          error = formatToolError(cause)
        }
        const archived = action.archived
        const archiveAction = compressionAction(action.action) ?? "auto_compact"
        const archiveTier = action.tier
        if (!error) this.activePageOutArchive = undefined
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "page_out_archive_result",
          effect_id: action.effectId,
          ...(archiveRef ? { archive_ref: archiveRef } : {}),
          ...(error ? { error } : {}),
        })
        if (!error && archiveTier === "semantic" && archived.length > 0) {
          void this.archiveSemanticPageOut(archived, archiveAction)
        }

      } else if (action.kind === "load_payload") {
        const content = await this.payloadStore().loadPayload(sessionId, action.payloadRef)
        action = content === undefined
          ? await this.commitKernelAction(runtime, this.pendingObservations, {
              kind: "payload_load_failed",
              effect_id: action.effectId,
              error: `payload not found for opaque locator ${action.payloadRef}`,
            })
          : await this.commitKernelAction(runtime, this.pendingObservations, {
              kind: "payload_loaded",
              effect_id: action.effectId,
              handle_id: action.handleId,
              payload: {
                content,
                digest: canonicalSha256(content),
                original_size: String(new TextEncoder().encode(content).byteLength),
              },
            })

      } else if (action.kind === "execute_tool") {
        const toolEffectId = action.effectId
        const allCalls = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        const runCtx: RunContext = {
          agentId: this.opts.agentId,
          memoryScope: this.opts.memoryScope,
          skillContentMap: this.opts.skillContentMap,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          onToolSuspend: this.opts.onToolSuspend,
          onPermissionRequest: this.opts.onPermissionRequest,
        }

        const toolResults: ToolResult[] = []
        // Syscall tools are consumed by core from the provider result and must never escape as host
        // tool effects. Keep an explicit invariant check below so a projection drift fails closed.
        const submitCalls = allCalls.filter(c => c.name === "submit_workflow_nodes" || c.name === "start_workflow")
        const planCalls = allCalls.filter(c => c.name === "update_plan")
        const normalCalls = allCalls.filter(
          c => c.name !== "submit_workflow_nodes" && c.name !== "start_workflow"
            && c.name !== "update_plan",
        )
        // `update_plan` is a kernel meta-tool (exposed via `enablePlanTool`), not a registered
        // plane tool — resolve it here as an `update_task` apply, mirroring the node/python runners.
        for (const call of planCalls) {
          const update = parseUpdatePlanArgs(call.arguments)
          await this.commitKernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel(update),
          })
          toolResults.push({ callId: call.id, output: "success", isError: false })
          yield { type: "tool_result", callId: call.id, content: "success", isError: false } as ToolResultEvent
        }
        for (const call of submitCalls) {
          throw new Error(
            `canonical kernel published model syscall ${call.name} as a host tool effect`,
          )
        }
        // O5 (PreToolUse-hook analog): stateful host veto over each kernel-approved call.
        // A blocked call never executes; its reason reaches the model as a denied result.
        let executableCalls = normalCalls
        if (this.opts.onToolCall) {
          const allowed: ToolCall[] = []
          for (const call of normalCalls) {
            let decision: ToolCallHookDecision | undefined | void
            try {
              decision = await this.opts.onToolCall({ callId: call.id, name: call.name, arguments: call.arguments })
            } catch { decision = undefined }
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
              arguments: typeof pre.arguments === "string" ? pre.arguments : JSON.stringify(pre.arguments),
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

        // O5 (PostToolUse-hook analog): host inspection of each executed result before it reaches
        // the kernel/session-log — replace the output and/or inject a signal note. Errs-open.
        if (this.opts.onToolResult) {
          for (const r of toolResults) {
            const call = executableCalls.find(c => c.id === r.callId)
            if (!call) continue
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
        // P1-B B3: a successfully-resolved `skill` call activates that skill for the next turn.
        //
        // Strict dynamic context control: a skill is METHOD content — how to do something — reused
        // for the rest of the run, unlike a one-off memory/knowledge lookup (fact content, relevant
        // for the moment it's used). So its text ALSO goes into the durable `knowledge` slot here
        // (in addition to the ordinary tool_result already headed for `history`, where it will decay
        // with the compression pyramid like any other tool output). First activation only.
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
            // With a lease configured, skip the Set optimization: an expired-then-reloaded skill
            // must re-pin — only the kernel knows the lease state; its upsert dedupes anyway.
            if (this.opts.skillLeaseTurns !== undefined || !this.knowledgePushedSkills.has(name)) {
              this.knowledgePushedSkills.add(name)
              // K1: keyed `skill:<name>` — the kernel-side upsert dedupes across runner instances.
              this.pushKnowledge({ role: "system", content: res.output }, undefined, { key: `skill:${name}` })
            }
          } catch { /* skip */ }
        }
        const entropyObsStart = this.pendingObservations.length
        action = await this.commitKernelAction(runtime, this.pendingObservations, {
          kind: "tool_results",
          effect_id: toolEffectId,
          results: toolResults.map(toolResultToKernel),
        })
        // Surface the boundary's entropy measurement live (the heartbeat watch source).
        for (const obs of this.pendingObservations.slice(entropyObsStart)) {
          if (obs.kind === "entropy_sample") {
            this.lastEntropySample = {
              turn: obs.turn ?? 0,
              score: obs.score ?? 0,
              scoreVersion: obs.score_version ?? 0,
              rho: obs.rho ?? 0,
              repeatPressure: obs.repeat_pressure ?? 0,
              failureRate: obs.failure_rate ?? 0,
              rollbacksInWindow: obs.rollbacks_in_window ?? 0,
              windowTurns: obs.window_turns ?? 0,
            }
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
        const milestonePhaseId = action.phaseId
        const milestonePolicy = this.opts.milestonePolicy ?? "require_verifier"
        if (milestonePolicy === "auto_pass") {
          action = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: action.effectId,
            result: milestoneCheckResultToKernel(milestoneCheckPass(action.phaseId)),
          })
          nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
        } else if (this.opts.onMilestoneEvaluate) {
          const check = await this.opts.onMilestoneEvaluate({
            phaseId: action.phaseId,
            criteria: action.criteria ?? [],
            requiredEvidence: action.requiredEvidence ?? [],
          })
          action = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: action.effectId,
            result: milestoneCheckResultToKernel(check),
          })
          nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
        } else {
          // R-B27: resolve the effect before ending the run. Nothing here can attest the phase, but
          // the kernel is holding this milestone in its pending-effect table and only a matching
          // result removes it — a bare `return` leaves a dangling effect that a logical-checkpoint
          // recovery cannot resolve. Feed back the conservative "unverified" resolution: the wire
          // has no error field yet, so it rides as `passed: false`, which keeps the phase where it
          // is (fail-closed, no unlocks mounted). The run still ends as `milestone_pending`.
          await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: milestoneEffectId,
            result: milestoneCheckResultToKernel(
              milestoneCheckFail(milestonePhaseId, MILESTONE_UNVERIFIED_REASON),
            ),
          })
          nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
          const turnsUsed = Math.max(1, runtime.turn())
          await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
            reason: "milestone_pending",
            turnsUsed,
            totalTokens: 0,
          }))
          yield { type: "done", iterations: turnsUsed, totalTokens: 0, status: "milestone_pending" } as DoneEvent
          this.activeKernel = null
          this.currentSessionId = null
          return
        }

      } else if (action.kind === "spawn_workflow") {
        await this.driveWorkflow(action, [], sessionId, runtime, new Map())
        action = runtime.resumeAction()
          ?? (() => { throw new Error("canonical workflow completed without a terminal or continuation") })()

      } else if (action.kind === "done") {
        break
      } else {
        // R-B28: fail-closed backstop. Without it an effect that reaches this position but has no
        // branch here (`spawn_workflow` / `preempt_sub_agents` are only driven inside the workflow
        // driver, and any effect kind a newer kernel adds) leaves `action` unreplaced and no event
        // in flight — `while (!runtime.isTerminal())` re-enters immediately and the run pins a core
        // at 100% forever while the kernel waits for a result that will never come. Terminating
        // through the loop's existing throw path makes the protocol mismatch visible instead
        // (run_terminal `error` + an `error` event) and cannot busy-wait.
        throw new Error(
          `unhandled kernel effect ${(action as { kind: string }).kind} in the main run loop`,
        )
      }
    }
    } catch (err) {
      // I0b: kernel rejection (or any other thrown error inside the loop) is observable here —
      // emit run_terminal so downstream code sees a clean end rather than mid-loop EOF.
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
      yield { type: "done", iterations: runtime.turn() || 0, totalTokens: 0, status: reason } as DoneEvent
      this.activeKernel = null
      this.currentSessionId = null
      return
    }

    const result = action.kind === "done" ? action.result : undefined
    // I0a: preserve preempt intent when loop exits without clean kernel-done (see Node runner for full rationale).
    const status = result?.termination ?? "error"
    const turnsUsed = result ? Math.max(1, result.turnsUsed) : runtime.turn() || 0
    const totalTokens = result?.totalTokensUsed ?? 0

    nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
    await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
      reason: status,
      turnsUsed,
      totalTokens,
    }))

    if (this.opts.dreamStore && this.opts.agentId) {
      const newMsgs = runtime.drainNewMessages().map(m => ({
        role: m.role,
        content: m.content,
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
            for (const memory of extracted) await this.writeMemory(memory, sessionId)
          }
        } catch { /* non-fatal */ }
      }
    }

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
  }

  /**
   * G3: run one workflow node, enforcing its `output_schema` (if any) by instructing the agent,
   * validating its output (the supported JSON-Schema subset), and re-running once with the errors
   * fed back on mismatch. If it still does not conform, the node is failed with the validation
   * reason (an `Error`-terminated result fails the node in-kernel, starving its dependents).
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
    // G4: surface remaining workflow budget so a coordinator node can size its submission.
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
      // This child is a workflow node, so capability resolution applies workflow-node quarantine
      // semantics instead of treating it as an independently spawned agent.
      isWorkflowNode: true,
      // W-N1: trusted workflow nodes run on the parent's execution plane (they carry no grant list
      // by design — filtering on the missing list ran every DAG node TOOL-LESS); quarantined nodes
      // stay deny-all filtered (they read untrusted content).
      toolAccess: (node.trust === "quarantined" ? "filtered" : "inherit") as "filtered" | "inherit",
      // #2-B-ii: the per-node abort signal the driver fires when the kernel preempts this node.
      ...(abortSignal ? { abortSignal } : {}),
    })
    const textOf = (r: SubAgentResult): string => {
      const c = r.result.finalMessage?.content
      return typeof c === "string" ? c : c != null ? JSON.stringify(c) : ""
    }
    const withSignal = (r: SubAgentResult, patch: Partial<SubAgentResult["result"]>): SubAgentResult =>
      ({ ...r, result: { ...r.result, ...patch } })

    // A#2 tournament judge: compare two entrants' produced outputs rather than running the node's own
    // goal. Look up both candidates, judge over the controller's criterion, and report the winner's id.
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

    // A#2 classify: run the classifier, then extract the chosen branch label (kernel prunes the rest).
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
   * G2: execute a deterministic reduce node — run the named reducer (built-ins overlaid with
   * `opts.reducers`) over its dependency outputs and return a synthetic completion. No LLM, zero
   * tokens. An unknown reducer or a thrown reducer fails the node (`Error` → starves dependents).
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
   */
  /**
   * Lower the declarative governance / attention / scheduler-budget / resource-quota / memory policies
   * into a freshly-created kernel. Shared by `execute()` (full run) and `initializeWorkflowKernel()`
   * (standalone workflow) so a DAG's node spawns are gated and quota'd exactly as a mid-run spawn.
   * Must run before the canonical root start. No config ⇒ native-profile defaults.
   */
  private async applyKernelPolicies(runtime: CanonicalRunnerRuntime): Promise<void> {
    const osProfile = assertNativeProfile(this.opts.osProfile ?? "native")
    const signalPolicy = this.opts.signalPolicy ?? osProfile.signalPolicy
    const governancePolicy = this.opts.governancePolicy ?? osProfile.governancePolicy

    // K2: lower governance / attention / scheduler / quota in ONE `configure_run` event (the 0.2.30
    // core applies each present field via its granular path). `set_memory_policy` stays separate below.
    const { kind: _govKind, ...governance } = governancePolicyToKernelEvent(governancePolicy) as Record<string, unknown>
    const config: Record<string, unknown> = { governance }
    if (this.opts.contextPolicy) {
      config.context_policy = normalizeContextPolicyV1(contextPolicyV1(this.opts.contextPolicy))
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
    if (this.opts.kernelReliability) {
      config.reliability = kernelReliabilityToKernel(this.opts.kernelReliability)
    }
    if (this.opts.resourceQuota) {
      const q = this.opts.resourceQuota
      config.resource_quota = {
        ...(q.maxConcurrentSubagents !== undefined ? { max_concurrent_subagents: q.maxConcurrentSubagents } : {}),
        ...(q.maxSpawnDepth !== undefined ? { max_spawn_depth: q.maxSpawnDepth } : {}),
        ...(q.maxWorkflowNodes !== undefined ? { max_workflow_nodes: q.maxWorkflowNodes } : {}),
        ...(q.memoryWritesPerWindow !== undefined
          ? { memory_writes_per_window: [q.memoryWritesPerWindow.maxWrites, q.memoryWritesPerWindow.windowMs] }
          : {}),
      }
    }
    // O6: tune/disable the in-kernel repeat fuse (absent ⇒ kernel defaults: enabled, 5/8).
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
    // P1: fail-closed dispatch selector (absent ⇒ kernel default "exposed"). "registered" is the
    // escape hatch back to permissive dispatch; the kernel rejects any other value.
    if (this.opts.toolDispatchGate !== undefined) {
      config.tool_dispatch_gate = this.opts.toolDispatchGate
    }
    // K2: knowledge budget ratio (absent ⇒ kernel default 0.25; 0 disables).
    if (this.opts.knowledgeBudgetRatio !== undefined) {
      config.knowledge_budget_ratio = this.opts.knowledgeBudgetRatio
    }
    // Entropy watch (opt-in): threshold alerting over the per-turn session-entropy score.
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
    if (this.opts.memoryPolicy) {
      await this.commitKernelApply(runtime, this.pendingObservations, {
        kind: "set_memory_policy",
        ...memoryPolicyToKernel(this.opts.memoryPolicy),
      })
    }
  }

  /**
   * Bootstrap a standalone kernel for a host-driven workflow with no active parent run. The caller
   * durably records `run_started` with the same `runId` before this method can create journal state.
   * `StartOperation { Workflow }` is the sole root transition; `runWorkflow` tears the kernel down.
   */
  private async initializeWorkflowKernel(sessionId: string, runId: string): Promise<CanonicalRunnerRuntime> {
    this.interrupted = false
    this.cancellationReason = undefined
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId

    const runtime = await this.createCanonicalRuntime(runId, sessionId)
    this.activeKernel = runtime

    await this.applyKernelPolicies(runtime)
    return runtime
  }

  async runWorkflow(
    spec: WorkflowSpec,
    opts?: {
      /** Standalone session id when bootstrapping (no active parent run). Defaults to a fresh uuid. */
      sessionId?: string
    },
  ): Promise<WorkflowOutcome> {
    // Standalone entry: with no active parent run, auto-bootstrap a kernel that owns the DAG (same
    // governance/quota policies a full run gets), drive it, then tear it down so the runner is reusable.
    // Mid-run callers keep the original in-place behavior with no teardown.
    const bootstrapped = !this.activeKernel || !this.currentSessionId
    if (bootstrapped) {
      const sessionId = opts?.sessionId ?? `wf-${crypto.randomUUID()}`
      const runId = crypto.randomUUID()
      await this.opts.sessionLog.append(sessionId, {
        kind: "run_started",
        run_id: runId,
        goal: `workflow:${spec.nodes.length} nodes`,
        criteria: [],
        ...(this.opts.agentId ? { agent_id: this.opts.agentId } : {}),
      })
      await this.initializeWorkflowKernel(sessionId, runId)
    }
    const parentSessionId = this.currentSessionId!
    const runtime = this.activeKernel!

    try {
      const observationStart = this.pendingObservations.length
      const initialAction = await canonicalStartWorkflow(
        runtime,
        this.pendingObservations,
        workflowSpecToKernel(spec),
      )
      const observations = this.pendingObservations.slice(observationStart)
      const outcome = await this.driveWorkflow(initialAction, observations, parentSessionId, runtime, new Map())
      if (bootstrapped) {
        let terminal = runtime.resumeAction()
        if (!terminal) throw new Error("completed canonical workflow has no terminal action")
        if (terminal.kind !== "done" && this.interrupted) {
          terminal = await this.commitKernelAction(runtime, this.pendingObservations, {
            kind: "cancel_operation",
            effect_id: terminal.effectId,
            reason: this.cancellationReason ?? "user",
          })
        }
        if (terminal.kind !== "done") {
          throw new Error("canonical workflow did not produce a terminal kernel action")
        }
        await this.appendObservations(parentSessionId, runtime, 0)
      }
      return outcome
    } finally {
      if (bootstrapped) {
        this.activeKernel = null
        this.currentSessionId = null
        this.abortController = null
        this.pendingObservations = []
      }
    }
  }

  /**
   * #2-B-ii: while a workflow batch is in flight, poll the signal source; a Critical `InterruptNow`
   * routes through the kernel (root in `SubAgentAwait` → preempt → `AgentPreempted` + tears the
   * `WorkflowRun` down), and we abort the matching children's in-flight LLM calls. Returns the
   * torn-down outcome on preemption, else `null`. No-op without a signal source.
   */
  private async monitorWorkflowPreemption(
    runtime: CanonicalRunnerRuntime,
    controllers: Map<string, AbortController>,
    batchState: { settled: boolean },
  ): Promise<WorkflowNodeOutcome[] | null> {
    const source = this.opts.signalSource
    if (!source && this.injectedSignals.length === 0) return null
    while (!batchState.settled) {
      // O2: injected notes participate in the monitor too (drain order matches nextInboundSignal).
      const delivery = await this.nextInboundSignal()
      if (batchState.settled) break
      if (!delivery) { await new Promise(resolve => setTimeout(resolve, 5)); continue }
      const observationStart = this.pendingObservations.length
      const signalAction = await this.consumeInboundSignal(delivery, claimed =>
        this.commitKernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(claimed)))
      if (signalAction) {
        if (signalAction.kind !== "preempt_sub_agents") {
          throw new Error(`workflow signal returned unexpected effect: ${signalAction.kind}`)
        }
        for (const id of signalAction.agentIds) controllers.get(id)?.abort()
        const continuation = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
          kind: "preempt_result", effect_id: signalAction.effectId,
        })
        if (continuation && continuation.kind !== "call_provider" && continuation.kind !== "done") {
          throw new Error(`workflow preemption returned unexpected effect: ${continuation.kind}`)
        }
      }
      const obs = this.pendingObservations.slice(observationStart)
      const preempted = obs.find(o => o.kind === "agent_preempted") as { agent_ids?: string[] } | undefined
      if (preempted) {
        for (const id of preempted.agent_ids ?? []) controllers.get(id)?.abort()
        const wc = obs.find(o => o.kind === "workflow_completed") as
          | { node_outcomes?: KernelWorkflowNodeOutcome[] }
          | undefined
        return (wc?.node_outcomes ?? []).map(workflowNodeOutcomeFromKernel)
      }
    }
    return null
  }

  /**
   * Shared canonical workflow driver: run each kernel-emitted batch in parallel, feed completions
   * back, and loop until the kernel reports the workflow complete.
   */
  private async driveWorkflow(
    initialAction: KernelRunnerAction | null,
    initial: KernelObservation[],
    parentSessionId: string,
    runtime: CanonicalRunnerRuntime,
    seedOutputs?: Map<string, string>,
  ): Promise<WorkflowOutcome> {
    const observations = initial
    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator

    const findDone = (obs: typeof observations) =>
      obs.find(o => o.kind === "workflow_completed") as { node_outcomes?: KernelWorkflowNodeOutcome[] } | undefined

    const acceptSpawn = async (spawn: Extract<KernelRunnerAction, { kind: "spawn_workflow" }>): Promise<KernelObservation[]> => {
      const observationStart = this.pendingObservations.length
      const continuation = await this.commitKernelMaybeAction(runtime, this.pendingObservations, {
        kind: "workflow_spawn_result",
        effect_id: spawn.effectId,
        started_agent_ids: spawn.nodes.map(node => String(node.agent_id ?? "")),
        failures: [],
      })
      if (continuation) throw new Error(`workflow spawn acknowledgement returned unexpected effect: ${continuation.kind}`)
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
    await acceptSpawn(initialAction)
    // G2: each completed node's output, keyed by agent id — a reduce node reads its deps' outputs.
    // W-1: on resume it is pre-seeded from the persisted node outputs, so post-resume dependents
    // still see their (pre-crash) dependencies' outputs.
    const outputs = new Map<string, string>(seedOutputs ?? [])

    for (;;) {
      if (nodes.length === 0) return { nodeOutcomes: [], outputs: Object.fromEntries(outputs) }

      for (const node of nodes) {
        const dependencyOutputs = (node as WorkflowSpawnInfo & {
          dependency_outputs?: Record<string, string>
        }).dependency_outputs ?? {}
        for (const [agentId, output] of Object.entries(dependencyOutputs)) {
          if (!outputs.has(agentId)) outputs.set(agentId, output)
        }
      }

      const roundBudget = budget
      // #2-B-ii: per-node abort controllers + a concurrent preemption monitor (see node runner).
      const controllers = new Map(nodes.map(n => [n.agent_id, new AbortController()] as const))
      const batchState = { settled: false }
      const monitor = this.monitorWorkflowPreemption(runtime, controllers, batchState)
      const results = await Promise.all(
        nodes.map(node => this.runWorkflowNode(node, parentSessionId, orchestrator, roundBudget, outputs, controllers.get(node.agent_id)?.signal)),
      )
      batchState.settled = true
      const preempted = await monitor
      if (preempted) return { nodeOutcomes: preempted, outputs: Object.fromEntries(outputs) }

      // Accumulate next-batch nodes across feeds (per-node unblock can spawn dependents per feed).
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
        } else if (completionAction?.kind === "done") {
          // The correlated child completion may terminalize the workflow in the same canonical
          // step. Its workflow_completed observation below remains the typed outcome source.
        } else if (completionAction) {
          throw new Error(`workflow completion returned unexpected effect: ${completionAction.kind}`)
        }
        // Child-authored DAG additions are admitted only as part of the canonical child-completion
        // resolution. Persist the projection only after core reports the admitted base index.
        if (result.submittedNodes?.length) {
          const submitted = obs.find(o => o.kind === "workflow_nodes_submitted") as
            | { base?: number }
            | undefined
          if (submitted) {
            await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
              turn: runtime.turn(),
              nodes: result.submittedNodes.map(workflowNodeSpecToKernel),
              baseIndex: submitted.base,
              submitterAgentId: result.agentId,
            }))
          }
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

  private async appendObservations(
    sessionId: string,
    runtime: CanonicalRunnerRuntime,
    nextArchiveStart: number,
  ): Promise<number> {
    const turn = runtime.turn()
    const preservedRefs = runtime.preservedRefs()
    const observations = this.pendingObservations.splice(0)
    for (const obs of observations) {
      if (obs.kind === "page_in_requested") continue

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
      // re-run the preQueryMemory prefetch for the new sprint (live observations only).
      if (obs.kind === "renewed") {
        await this.prefetchMemoryIntoHistory(runtime, "renewal")
      }
    }
    return nextArchiveStart
  }

  /** I4 + K4: fetch long-term memory hits for the current goal and land them in `history` as an
   *  ordinary user turn — single-use retrieval content that decays with the compression pyramid,
   *  never pinned into `knowledge`. `phase: "initial"` = once before turn 1; `phase: "renewal"` =
   *  re-fired after each sprint renewal (renewal drops the old history INCLUDING earlier memory
   *  hits). Errs-open throughout. */
  private async prefetchMemoryIntoHistory(
    runtime: CanonicalRunnerRuntime,
    phase: "initial" | "renewal",
  ): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId || !this.opts.memoryScope) return
    // P10: recall is default-on (CC session-start recall) — with no hook configured,
    // the goal itself is the query. preQueryMemory stays as the targeting override.
    const preQuery = this.opts.preQueryMemory
      ?? ((ctx: { goal: string }) => [{ scope: this.opts.memoryScope!, query: ctx.goal, top_k: 5, kinds: [] }])
    try {
      const queries = await preQuery({ goal: this.currentGoal, phase })
      const lines: string[] = []
      for (const q of queries ?? []) {
        if (!q.query.trim()) continue
        const hits = await this.opts.dreamStore.search(this.opts.agentId, q)
        for (const hit of hits) {
          lines.push(`[memory record_id=${hit.record.record_id} trust=${hit.record.provenance.trust} score=${hit.score.toFixed(3)}] ${hit.record.content}`)
        }
      }
      if (lines.length > 0) {
        await this.commitKernelApply(runtime, this.pendingObservations, {
          kind: "add_history_message",
          message: { role: "user", content: lines.join("\n") },
        })
      }
    } catch { /* errs-open */ }
  }

  private async archiveSemanticPageOut(archived: Message[], action?: string): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId || !this.opts.memoryScope) return
    try {
      const summary = this.opts.dreamSummarizer
        ? await this.opts.dreamSummarizer.summarize(archived, { action })
        : await summarizeForLongTermMemory(
          this.opts.dreamProvider ?? this.opts.provider,
          archived,
          this.opts.dreamSystemPrompt,
        )
      const now = Date.now()
      const name = `page-out-${now}`
      await this.writeMemory({
          record_id: `${this.opts.memoryScope.tenant_id}:${this.opts.memoryScope.namespace}:project:${name}`,
          scope: this.opts.memoryScope, name, kind: "project", content: summary,
          description: `auto summary of ${action ?? "compaction"} archive`,
          provenance: { author: "extraction", trust: "untrusted", evidence_refs: [] },
          created_at: now, updated_at: now, recall_count: 0, confidence: 0.6, links: [], pinned: false,
      }, this.currentSessionId ?? undefined)
    } catch {
      // non-fatal
    }
  }
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
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  return text.trim() || transcript.slice(0, 2000)
}

function isMidRun(events: Array<{ seq: number; event: SessionEvent }>): boolean {
  return events.length > 0 && !events.some(e => e.event.kind === "run_terminal")
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

async function replayMessages(
  events: Array<{ seq: number; event: SessionEvent }>,
  maxBytes?: number,
  archiveStore?: ArchiveStore,
): Promise<Message[]> {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
  const archivedTurns = new Set(events.flatMap(({ event }) =>
    event.kind === "page_out" && event.archive_ref && archiveStore?.read ? [event.turn] : [],
  ))
  for (const { seq, event: e } of events) {
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
      if (archivedTurns.has(e.turn)) continue
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
    } else if (e.kind === "page_out" && e.archive_ref && archiveStore?.read) {
      try {
        const archived = await archiveStore.read(e.archive_ref)
        messages.push(...archived.map(message => ({
          ...message,
          content: sanitizeReplayText(message.content, maxBytes),
        })))
      } catch {
        if (e.summary) {
          const systemText = `[Compressed context: turn ${e.turn}]\n${e.summary}`
          messages.push({
            role: "system", content: systemText, toolCalls: [],
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
          content: sanitizeReplayText(r.output, maxBytes),
          toolCalls: [],
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
  return messages
}

function nextArchivedSeqStart(events?: Array<{ seq: number; event: SessionEvent }>): number {
  let next = 0
  for (const { event } of events ?? []) {
    if (event.kind === "compressed") next = Math.max(next, event.archived_seq_range[1] + 1)
  }
  return next
}


export async function collectText(stream: AsyncIterable<StreamEvent>): Promise<string> {
  let text = ""
  for await (const evt of stream) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  return text
}

/** Parse `update_plan` meta-tool args into a task update (snake_case aliases accepted, mirroring
 *  the node/python runners). Malformed payload → an empty update (a no-op `update_task`). */
function parseUpdatePlanArgs(argsStr: string): Parameters<typeof taskUpdateToKernel>[0] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → empty update.
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

/** Lower a claimed signal delivery to the kernel's `deliver_signal` input event. Shared by the main
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

/**
 * True when an earlier run in this session already seeded the same attachments. Replay
 * reconstructs the attachment message from that run's `run_started`, so recording and
 * live-seeding them again (a same-session retry attempt) would double them in history.
 */
function attachmentsAlreadySeeded(
  prior: Array<{ seq: number; event: SessionEvent }>,
  attachments: import("../types.js").ContentPart[],
): boolean {
  const wanted = JSON.stringify(attachments)
  return prior.some(({ event }) =>
    event.kind === "run_started" && JSON.stringify(event.attachments ?? []) === wanted)
}

/** Convert SDK ContentParts (camelCase mediaType) to kernel serde shape (media_type). */
function attachmentsToKernelMessage(parts: import("../types.js").ContentPart[]): Record<string, unknown> {
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
