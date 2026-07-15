import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, WorkflowNodesSubmittedEvent, DoneEvent, ErrorEvent,
  ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  EntropySample, EntropySampleEvent, EntropyAlertEvent, EntropyWatchOptions,
  DreamSummarizer,
} from "../types.js"
import type { ToolSuspendEvent } from "./execution-plane.js"
import type { DreamStore, DreamResult, MemoryEntry, CurationResult, SessionData } from "../memory/index.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import type { SignalSource, RuntimeSignal, SignalDeliveryReceipt } from "../signals/index.js"
import type { SessionLog, SessionEvent } from "./session-log.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { governancePolicyToKernelEvent, governanceFilterSchema, type GovernancePolicy } from "../governance.js"
import { getKernel } from "./kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import { formatToolError } from "../tools/errors.js"
import {
  buildLlmCompletedEvent,
  buildRunTerminalEvent,
  buildWorkflowNodeCompletedEvent,
  buildWorkflowNodesSubmittedEvent,
  recoverCompletedWorkflowNodes,
  recoverSubmittedWorkflowNodes,
  repairEventsForRecovery,
  type RecoveredNodeCompletion,
} from "./session-repair.js"
import {
  kernelAction,
  kernelApply,
  kernelMaybeAction,
  messageToKernelMessage,
  skillMetadataToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
  type KernelObservation,
  type KernelRunnerAction,
  type KernelRuntimeHandle,
} from "./kernel-step.js"
import type { AgentRunSpec, AgentProcessChangedObservation, SubAgentResult, MilestonePolicy, MilestoneContract, MilestoneCheckResult, WorkflowSpec, WorkflowSpawnInfo, WorkflowNodeSpec, WorkflowBudget } from "./types/agent.js"
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
import { assertNativeProfile, type NativeOsProfile, type OsProfileId } from "./os-profile.js"
import { LargeResultSpool } from "./large-result-spool.js"

export interface MemoryWriteRateLimit {
  maxWrites: number
  windowMs: number
}

export interface ResourceQuota {
  /** Max sub-agents in the `running` state at once; further spawns are denied while at cap. */
  maxConcurrentSubagents?: number
  /** Max sub-agent nesting depth (direct children of the root loop are depth 1). */
  maxSpawnDepth?: number
  /** Rolling-window memory-write rate limit: at most `maxWrites` per any `windowMs` span. */
  memoryWritesPerWindow?: MemoryWriteRateLimit
}

export interface SchedulerBudget {
  maxWallMs?: number
}

/**
 * Long-term memory policy (`set_memory_policy`) — opt-in, kernel-enforced. `validationEnabled:
 * false` admits writes without validation, `maxContentBytes` / `maxNameLength` override the
 * validation limits, and `retrievalTopK` caps `query_memory` breadth. `memoryPath` /
 * `staleWarningDays` are carried for SDK recall I/O. Omitted fields keep the kernel defaults.
 */
export interface MemoryPolicy {
  memoryPath?: string
  staleWarningDays?: number
  retrievalTopK?: number
  validationEnabled?: boolean
  maxContentBytes?: number
  maxNameLength?: number
}

export interface KernelReliabilityOptions {
  eventReplayCapacity?: number
  completedEffectReplayCapacity?: number
  providerRecoveryAttempts?: number
  outputRecoveryAttempts?: number
  hostEffectRetryAttempts?: number
  spoolThresholdBytes?: number
  spoolPreviewBytes?: number
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
  /** I4: optional run-start memory pre-fetch hook (mirrors Node SDK). Called once per run before
   *  the first LLM turn; each returned query string becomes a dreamStore search; hits page into
   *  the knowledge partition before turn 1. Requires dreamStore + agentId. */
  preQueryMemory?: (ctx: {
    goal: string
    /** K4: `"initial"` = pre-turn-1 fetch; `"renewal"` = re-fired after a sprint renewal. */
    phase?: "initial" | "renewal"
  }) => Promise<string[] | undefined> | string[] | undefined
  systemPrompt?: string
  initialMemory?: string[]
  /** Skill name → markdown body (WASM has no filesystem). */
  skillContentMap?: Map<string, string>
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  /** Named or concrete OS profile. Defaults to the native microkernel profile. */
  osProfile?: OsProfileId | NativeOsProfile
  governancePolicy?: GovernancePolicy
  attentionPolicy?: { maxQueueSize?: number }
  schedulerBudget?: SchedulerBudget
  kernelReliability?: KernelReliabilityOptions
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
  /** M5 v2.1: marks this runner as a workflow node (child of the workflow driver). A workflow node's
   *  `start_workflow` FLATTENS to the parent kernel; a top-level run (unset) AUTO-PIVOTS — bootstraps +
   *  drives the authored workflow in its own kernel, then resumes the reason loop with the outcome. */
  isWorkflowNode?: boolean
  /** G2: custom reducers for `NodeKind::Reduce` workflow nodes, merged over the built-ins. */
  reducers?: ReducerRegistry
  milestonePolicy?: MilestonePolicy
  milestoneContract?: MilestoneContract
  onMilestoneEvaluate?: (ctx: { phaseId: string; criteria: string[]; requiredEvidence: string[] }) => Promise<MilestoneCheckResult> | MilestoneCheckResult
  runSpec?: AgentRunSpec
  /** P0-A tool gating: a static per-run tool profile — only these tool ids (plus the meta-tools)
   *  are exposed to the model each turn. Lowers to the same `capability_filter` sub-agents use;
   *  byte-stable across the run, so it never busts the prompt-cache prefix. Augments `runSpec`'s
   *  filter when both set; synthesizes a minimal spec otherwise. Omitted/empty ⇒ no gating. */
  allowedToolIds?: string[]
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
  resultSpool?: LargeResultSpool
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

export class RuntimeRunner {
  private interrupted = false
  private cancellationReason: OperationCancellationReason | undefined
  /** #2-B-ii: aborts the in-flight provider stream on interrupt/preempt. Recreated per `execute`. */
  private abortController: AbortController | null = null
  private pendingObservations: KernelObservation[] = []
  private activeKernel: KernelRuntimeHandle | null = null
  private currentSessionId: string | null = null
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
  /** M5 v2.1: sub-workflow specs a top-level agent authored via `start_workflow`, awaiting auto-drive
   *  at the next safe point (after the tool turn resolves, kernel back in Reason). */
  private pendingAuthoredWorkflows: WorkflowSpec[] = []
  private workflowContinuation: Extract<KernelRunnerAction, { kind: "call_provider" }> | null = null

  constructor(private readonly opts: RuntimeOptions) {}

  get hostOptions(): RuntimeOptions { return this.opts }

  interrupt(reason: OperationCancellationReason = "user"): void {
    this.interrupted = true
    this.cancellationReason = reason
    this.abortController?.abort(reason)
  }

  /** Push a contextual note into the run's signal stream (the system-reminder channel): it drains at
   *  the next turn boundary, routes through the kernel attention policy, and — once acted on — renders
   *  as a `[SIGNAL] <text>` line in the volatile state turn plus a durable directive. `urgency` maps to
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
    consume: (delivery: InboundSignalDelivery) => T,
  ): Promise<T> {
    try {
      const observationStart = this.pendingObservations.length
      const result = consume(delivery)
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
    extensions?: Record<string, unknown>
    inheritEvents?: Array<{ seq: number; event: SessionEvent }>
  }): AsyncIterable<StreamEvent> {
    const prior = req.inheritEvents ?? await this.opts.sessionLog.read(req.sessionId)
    const midRun = isMidRun(prior)
    if (!midRun) {
      await this.opts.sessionLog.append(req.sessionId, {
        kind: "run_started",
        run_id: crypto.randomUUID(),
        goal: req.goal,
        criteria: req.criteria ?? [],
        agent_id: this.opts.agentId,
        system_prompt: this.opts.systemPrompt,
      })
    }
    yield* this.execute(
      req.sessionId,
      req.goal,
      req.criteria ?? [],
      req.extensions,
      prior.length > 0 ? prior : undefined,
      midRun,
    )
  }

  async *wake(sessionId: string, extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const events = await this.opts.sessionLog.read(sessionId)
    if (events.some(e => e.event.kind === "run_terminal")) return

    const startEntry = [...events].reverse().find(e => e.event.kind === "run_started")
    if (!startEntry) throw new Error(`No run_started event for session: ${sessionId}`)
    const start = startEntry.event as Extract<SessionEvent, { kind: "run_started" }>

    yield* this.execute(sessionId, start.goal, start.criteria, extensions, events, true)
  }

  /** Push content into Slot 2 (system_knowledge) via add_knowledge_message.
   *  K1: `opts.key` gives the entry identity — a same-key push upserts (applied at the next
   *  compaction/renewal boundary) instead of appending a duplicate. `opts.pinned` exempts the
   *  entry from the knowledge-budget sweep. */
  pushKnowledge(message: Message, tokens?: number, opts?: { key?: string; pinned?: boolean }): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, {
      kind: "add_knowledge_message",
      content: message.content ?? "",
      tokens: tokens ?? Math.max(1, Math.ceil((message.content?.length ?? 0) / 4)),
      ...(opts?.key !== undefined ? { key: opts.key } : {}),
      ...(opts?.pinned ? { pinned: true } : {}),
    })
  }

  /** K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
   *  Errs-open: an unknown key is a kernel-side no-op. */
  removeKnowledge(key: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, { kind: "remove_knowledge", key })
  }

  /** K3: host-driven skill deactivation — toolset re-widens at the next provider call, the
   *  skill's knowledge pin drops at the next boundary. Errs-open: not-active is a no-op. */
  deactivateSkill(name: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, { kind: "skill_deactivated", name })
    this.knowledgePushedSkills.delete(name)
  }

  private async resolveKernelSuspend(
    requests: Array<{ callId: string; tool: string; arguments: string; reason: string }>,
    runtime: KernelRuntimeHandle,
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

  /**
   * O7: resolve a `read_result` meta-tool call to the full text of a previously-evicted tool
   * output. Resolution order: (a) the result spool committed by `large_result_spool_result`,
   * (b) a session-log scan for the original `tool_completed` event carrying that `call_id`.
   * Slices the resolved text by `[offset, offset + maxBytes)` (plain string slice — "bytes-ish").
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

    if (full === undefined && this.opts.resultSpool) {
      try {
        full = await this.opts.resultSpool.findByCallId(callId)
      } catch {
        full = undefined
      }
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

  async dream(agentId: string, nowMs = Date.now()): Promise<DreamResult> {
    if (!this.opts.dreamStore) throw new Error("dreamStore not configured")
    const kernel = await getKernel()

    const sessions = await this.opts.dreamStore.loadSessions(agentId)
    const existingMemories = await this.opts.dreamStore.loadMemories(agentId)
    if (!sessions.length) return { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 }

    const pipeline = new kernel.IdlePipeline(agentId)
    const action1 = pipeline.feedTrigger(
      sessions.map(s => ({
        sessionId: s.sessionId, agentId: s.agentId,
        messages: s.messages.map(m => ({
          role: m.role, content: m.content, tokenCount: m.tokenCount,
          toolCalls: (m.toolCalls ?? []).map(tc => ({ id: tc.id, name: tc.name, arguments: tc.arguments })),
        })),
        metadata: JSON.stringify(s.metadata ?? null),
        createdAtMs: s.createdAtMs, updatedAtMs: s.updatedAtMs,
      })),
      existingMemories.map(e => ({ text: e.text, score: e.score, metadata: JSON.stringify(e.metadata ?? null) })),
      nowMs,
    )
    if (action1.kind === "noop" || action1.kind === "aborted") {
      return { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 }
    }
    if (action1.kind !== "synthesize_insights") throw new Error(`unexpected: ${action1.kind}`)

    let synthesisText = ""
    const dreamProvider = this.opts.dreamProvider ?? this.opts.provider
    const providerState = dreamProvider.createRunState?.()
    const synthMsgs = (action1.messages ?? []) as Message[]
    const synthContext = {
      systemText: synthMsgs.filter(m => m.role === "system").map(m => m.content).join("\n\n"),
      turns: synthMsgs.filter(m => m.role !== "system"),
    }
    for await (const evt of dreamProvider.stream(synthContext, [], undefined, providerState)) {
      if (evt.type === "text_delta") synthesisText += (evt as TextDelta).delta
    }

    const action2 = pipeline.feedSynthesisResult(synthesisText) as {
      kind: string
      curationResult?: {
        toAdd?: MemoryEntry[]
        toRemoveIndices?: number[]
        stats?: { insightsProcessed?: number; duplicatesRemoved?: number; conflictsResolved?: number; entriesAdded?: number }
      }
      runResult?: { sessionsProcessed: number; insightsExtracted: number }
    }
    if (action2.kind !== "commit_memories") throw new Error(`unexpected: ${action2.kind}`)
    const cr = action2.curationResult!
    const rr = action2.runResult!

    const dsResult: CurationResult = {
      toAdd: (cr.toAdd ?? []).map((e: MemoryEntry): MemoryEntry => ({
        text: e.text, score: e.score, metadata: tryParseJson(e.metadata as string),
      })),
      toRemoveIndices: (cr.toRemoveIndices ?? []).map(Number),
      stats: {
        insightsProcessed: cr.stats?.insightsProcessed ?? 0,
        duplicatesRemoved: cr.stats?.duplicatesRemoved ?? 0,
        conflictsResolved: cr.stats?.conflictsResolved ?? 0,
        entriesAdded: cr.stats?.entriesAdded ?? 0,
      },
    }
    await this.opts.dreamStore.commit(agentId, dsResult, existingMemories)

    return {
      sessionsProcessed: rr.sessionsProcessed,
      insightsExtracted: rr.insightsExtracted,
      entriesAdded: cr.stats?.entriesAdded ?? 0,
      entriesRemoved: (cr.toRemoveIndices ?? []).length,
    }
  }

  private async *execute(
    sessionId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.cancellationReason = undefined
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId
    const kernel = await getKernel()

    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    const providerPolicy = (this.opts.provider as { runtimePolicy?: () => { maxTurns?: number; timeoutMs?: number } }).runtimePolicy?.() ?? {}
    const effectiveMaxTurns = this.opts.maxTurns ?? providerPolicy.maxTurns ?? 25
    const effectiveTimeoutMs = this.opts.timeoutMs ?? providerPolicy.timeoutMs

    const runtime = new kernel.KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: effectiveMaxTurns,
      // M4/G5: per-node token cap → child run's cumulative token budget (wasm LoopPolicy.maxTotalTokens is f64).
      ...(this.opts.maxTotalTokens !== undefined ? { maxTotalTokens: this.opts.maxTotalTokens } : {}),
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
    })
    this.activeKernel = runtime

    if (this.opts.tokenizer) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_tokenizer",
        name: this.opts.tokenizer,
      })
    }
    if (this.opts.enablePlanTool !== undefined) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_plan_tool_enabled",
        enabled: this.opts.enablePlanTool,
      })
    }

    kernelApply(runtime, this.pendingObservations, {
      kind: "set_tools",
      tools: this.opts.executionPlane.schemas().map(toolSchemaToKernel),
    })

    if (this.opts.systemPrompt) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_system_message",
        content: this.opts.systemPrompt,
        tokens: Math.max(1, Math.ceil(this.opts.systemPrompt.length / 4)),
      })
    }

    if (this.opts.initialMemory) {
      for (const mem of this.opts.initialMemory) {
        kernelApply(runtime, this.pendingObservations, {
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
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_available_skills",
        skills: metas.map(skillMetadataToKernel),
      })
    }

    // P1-B/D: configure stable-core tool ids (always exposed under skill gating).
    if (this.opts.stableCoreToolIds?.length) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_stable_core_tools",
        tool_ids: this.opts.stableCoreToolIds,
      })
    }

    if (this.opts.dreamStore && this.opts.agentId) {
      kernelApply(runtime, this.pendingObservations, { kind: "set_memory_enabled", enabled: true })
    }
    if (this.opts.knowledgeSource) {
      kernelApply(runtime, this.pendingObservations, { kind: "set_knowledge_enabled", enabled: true })
    }

    if (this.opts.milestoneContract) {
      kernelApply(runtime, this.pendingObservations, {
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
      const repaired = repairEventsForRecovery(priorEvents, maxBytes)
      seedProviderReplayFromEvents(this.opts.provider, repaired)
      const replayed = await replayMessages(repaired, maxBytes, this.opts.compressionStore)
      kernelApply(runtime, this.pendingObservations, {
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
            kernelApply(runtime, this.pendingObservations, {
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

    const sessionStart = Date.now()
    const startPayload: Record<string, unknown> = {
      kind: "start_run",
      task: { goal, criteria },
    }
    // P0-A: lower an explicit `runSpec` and/or the `allowedToolIds` profile to the kernel's
    // `capability_filter` (reuses the existing run_spec wire — no new ABI). Unset on both ⇒ no
    // gating (铁律: no config = old behavior).
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
    this.applyKernelPolicies(runtime)

    // I4: pre-fetch memory before the first LLM turn (mirrors Node). Strict dynamic context
    // control: single-use retrieval content, not a stable skill — lands in `history` like an
    // ordinary `memory` tool result, so it decays with the compression pyramid instead of
    // pinning itself in `knowledge` forever.
    this.currentGoal = goal
    if (!resumeMidRun) {
      await this.prefetchMemoryIntoHistory(runtime, "initial")
    }

    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    // P0-C: the skill loaded and in effect going into the current turn → per-turn `activeSkill` metric.
    let activeSkill: string | undefined

    // I0b: kernel-throw safety net — see Node runner for full rationale.
    try {
    while (!runtime.isTerminal()) {
      nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
      if (this.interrupted) {
        action = kernelAction(runtime, this.pendingObservations, {
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
            kernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(claimed)))
          if (sigAction) action = sigAction
          // Critical attention/preemption is distinct from operation cancellation.
        }
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        // M5 v2.1: top-level auto-pivot at the safe point (kernel in Reason, not suspended). Loop-top
        // placement catches every path to `call_provider` (incl. post-approval-resume), so a queued
        // authored spec is never stranded. Drains the queue; fires once per authored batch.
        if (this.pendingAuthoredWorkflows.length > 0) {
          action = await this.driveAuthoredWorkflows(runtime, action)
        }
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
            action = kernelAction(runtime, this.pendingObservations, {
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
          action = kernelAction(runtime, this.pendingObservations, {
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
        action = kernelAction(runtime, this.pendingObservations, providerEvent)
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
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "approval_result",
          effect_id: action.effectId,
          approved_calls: resolved.approved,
          denied_calls: resolved.denied,
        })

      } else if (action.kind === "persist_memory") {
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "memory_persist_result",
          effect_id: action.effectId,
          error: "WASM host memory persistence requires an explicit SDK memory adapter",
        })

      } else if (action.kind === "query_memory") {
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "memory_query_result",
          effect_id: action.effectId,
          entries: [],
          error: "WASM host memory queries require an explicit SDK memory adapter",
        })

      } else if (action.kind === "spool_large_result") {
        const spool = this.opts.resultSpool ?? new LargeResultSpool()
        let spoolRef: string | undefined
        let error: string | undefined
        try {
          spoolRef = await spool.persistOutput(action.callId, action.output)
        } catch (cause) {
          error = formatToolError(cause)
        }
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "large_result_spool_result",
          effect_id: action.effectId,
          ...(spoolRef ? { spool_ref: spoolRef } : {}),
          ...(error ? { error } : {}),
        })

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
        } catch (cause) {
          error = formatToolError(cause)
        }
        const archived = action.archived
        const archiveAction = compressionAction(action.action) ?? "auto_compact"
        const archiveTier = action.tier
        if (!error) this.activePageOutArchive = undefined
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "page_out_archive_result",
          effect_id: action.effectId,
          ...(archiveRef ? { archive_ref: archiveRef } : {}),
          ...(error ? { error } : {}),
        })
        if (!error && archiveTier === "semantic" && archived.length > 0) {
          void this.archiveSemanticPageOut(archived, archiveAction)
        }

      } else if (action.kind === "execute_tool") {
        const toolEffectId = action.effectId
        const allCalls = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        const runCtx: RunContext = {
          agentId: this.opts.agentId,
          skillContentMap: this.opts.skillContentMap,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          onToolSuspend: this.opts.onToolSuspend,
          onPermissionRequest: this.opts.onPermissionRequest,
        }

        const toolResults: ToolResult[] = []
        // R3-1: intercept `submit_workflow_nodes` — it can't apply to this runner's kernel (when this
        // runner is a workflow node, the workflow lives in the parent). Surface the nodes as an event;
        // the orchestrator collects them and `runWorkflow` sends them to the parent kernel.
        // M5 v1: `start_workflow` (author a sub-workflow) flattens to the same append path.
        const submitCalls = allCalls.filter(c => c.name === "submit_workflow_nodes" || c.name === "start_workflow")
        // O7: `read_result` re-fetches a tool output the kernel evicted from context. Content is
        // host-resolved: (a) this turn's in-memory pending spool map, (b) the on-disk result spool,
        // (c) a session-log scan for the original `tool_completed` event.
        const readResultCalls = allCalls.filter(c => c.name === "read_result")
        const normalCalls = allCalls.filter(
          c => c.name !== "submit_workflow_nodes" && c.name !== "start_workflow" && c.name !== "read_result",
        )
        for (const call of readResultCalls) {
          const out = await this.resolveReadResult(sessionId, call.arguments)
          toolResults.push({ callId: call.id, output: out.text, isError: out.isError })
          yield { type: "tool_result", callId: call.id, content: out.text, isError: out.isError } as ToolResultEvent
        }
        for (const call of submitCalls) {
          // M5 v2.1: a TOP-LEVEL agent authoring a whole sub-workflow via `start_workflow` — record the
          // spec and AUTO-PIVOT once this tool turn resolves. A workflow-NODE's `start_workflow` (and
          // every `submit_workflow_nodes`) instead FLATTENS for the parent `runWorkflow` to append.
          if (call.name === "start_workflow" && !this.opts.isWorkflowNode) {
            const spec = parseStartWorkflowSpec(call.arguments)
            if (spec) {
              this.pendingAuthoredWorkflows.push(spec)
              const out = "workflow authored; executing now"
              toolResults.push({ callId: call.id, output: out, isError: false })
              yield { type: "tool_result", callId: call.id, content: out, isError: false } as ToolResultEvent
              continue
            }
          }
          const nodes = call.name === "start_workflow"
            ? parseStartWorkflowArgs(call.arguments)
            : parseSubmitWorkflowNodesArgs(call.arguments)
          yield { type: "workflow_nodes_submitted", nodes } as WorkflowNodesSubmittedEvent
          toolResults.push({ callId: call.id, output: "submitted", isError: false })
          yield { type: "tool_result", callId: call.id, content: "submitted", isError: false } as ToolResultEvent
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
            kernelApply(runtime, this.pendingObservations, {
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
        action = kernelAction(runtime, this.pendingObservations, {
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
        const milestonePolicy = this.opts.milestonePolicy ?? "require_verifier"
        if (milestonePolicy === "auto_pass") {
          action = kernelAction(runtime, this.pendingObservations, {
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
          action = kernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            effect_id: action.effectId,
            result: milestoneCheckResultToKernel(check),
          })
          nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
        } else {
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

      } else if (action.kind === "done") {
        break
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
          await this.opts.dreamStore.saveSession({
            sessionId: crypto.randomUUID(),
            agentId: this.opts.agentId,
            messages: newMsgs,
            metadata: null,
            createdAtMs: sessionStart,
            updatedAtMs: Date.now(),
          } as SessionData)
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

  async spawnSubAgent(spec: AgentRunSpec): Promise<SubAgentResult> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("spawnSubAgent requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel

    const observations = kernelApply(runtime, this.pendingObservations, {
      kind: "spawn_sub_agent",
      spec: agentRunSpecToKernel(spec),
      parent_session_id: parentSessionId,
    })
    this.nextArchiveStart = await this.appendObservations(parentSessionId, runtime, this.nextArchiveStart)

    const spawned = findSpawnProcessObservation(observations)
    if (!spawned) throw new Error("spawn_sub_agent did not emit agent_process_changed")

    const manifest = spawnObservationToManifest(spawned, spec, parentSessionId)

    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator
    const result = await orchestrator.run({
      parentOpts: this.opts,
      parentSessionId,
      spec,
      manifest,
      sessionLog: this.opts.sessionLog,
    })

    kernelApply(runtime, this.pendingObservations, {
      kind: "sub_agent_completed",
      result: subAgentResultToKernel(result),
    })
    return result
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
      // M5 v2.1: this child IS a workflow node — its `start_workflow` flattens to this kernel.
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

    const MAX_ATTEMPTS = 2
    let last: SubAgentResult | undefined
    let lastErrors: string[] = []
    for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
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

    const reason = `output_schema validation failed after ${MAX_ATTEMPTS} attempts: ${lastErrors.join("; ")}`
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
   * into a freshly-created kernel. Shared by `execute()` (full run) and `bootstrapWorkflowKernel()`
   * (standalone workflow) so a DAG's node spawns are gated and quota'd exactly as a mid-run spawn.
   * Must run BEFORE `start_run`. No config ⇒ native-profile defaults.
   */
  private applyKernelPolicies(runtime: KernelRuntimeHandle): void {
    const osProfile = assertNativeProfile(this.opts.osProfile ?? "native")
    const attentionPolicy = this.opts.attentionPolicy ?? osProfile.attentionPolicy
    const governancePolicy = this.opts.governancePolicy ?? osProfile.governancePolicy

    // K2: lower governance / attention / scheduler / quota in ONE `configure_run` event (the 0.2.30
    // core applies each present field via its granular path). `set_memory_policy` stays separate below.
    const { kind: _govKind, ...governance } = governancePolicyToKernelEvent(governancePolicy) as Record<string, unknown>
    const config: Record<string, unknown> = { governance }
    if (attentionPolicy.maxQueueSize !== undefined) {
      config.attention_max_queue_size = attentionPolicy.maxQueueSize
    }
    if (this.opts.schedulerBudget?.maxWallMs !== undefined) {
      config.scheduler_max_wall_ms = this.opts.schedulerBudget.maxWallMs
    }
    if (this.opts.kernelReliability) {
      const r = this.opts.kernelReliability
      config.reliability = {
        ...(r.eventReplayCapacity !== undefined ? { event_replay_capacity: r.eventReplayCapacity } : {}),
        ...(r.completedEffectReplayCapacity !== undefined ? { completed_effect_replay_capacity: r.completedEffectReplayCapacity } : {}),
        ...(r.providerRecoveryAttempts !== undefined ? { provider_recovery_attempts: r.providerRecoveryAttempts } : {}),
        ...(r.outputRecoveryAttempts !== undefined ? { output_recovery_attempts: r.outputRecoveryAttempts } : {}),
        ...(r.hostEffectRetryAttempts !== undefined ? { host_effect_retry_attempts: r.hostEffectRetryAttempts } : {}),
        ...(r.spoolThresholdBytes !== undefined ? { spool_threshold_bytes: r.spoolThresholdBytes } : {}),
        ...(r.spoolPreviewBytes !== undefined ? { spool_preview_bytes: r.spoolPreviewBytes } : {}),
      }
    }
    if (this.opts.resourceQuota) {
      const q = this.opts.resourceQuota
      config.resource_quota = {
        ...(q.maxConcurrentSubagents !== undefined ? { max_concurrent_subagents: q.maxConcurrentSubagents } : {}),
        ...(q.maxSpawnDepth !== undefined ? { max_spawn_depth: q.maxSpawnDepth } : {}),
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
    kernelApply(runtime, this.pendingObservations, { kind: "configure_run", config })
    if (this.opts.memoryPolicy) {
      const m = this.opts.memoryPolicy
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_memory_policy",
        ...(m.memoryPath !== undefined ? { memory_path: m.memoryPath } : {}),
        ...(m.staleWarningDays !== undefined ? { stale_warning_days: m.staleWarningDays } : {}),
        ...(m.retrievalTopK !== undefined ? { retrieval_top_k: m.retrievalTopK } : {}),
        ...(m.validationEnabled !== undefined ? { validation_enabled: m.validationEnabled } : {}),
        ...(m.maxContentBytes !== undefined ? { max_content_bytes: m.maxContentBytes } : {}),
        ...(m.maxNameLength !== undefined ? { max_name_length: m.maxNameLength } : {}),
      })
    }
  }

  /**
   * Bootstrap a standalone kernel for a host-driven workflow with no active parent run — the path a
   * stateless handler (browser/edge worker) takes when it calls `runWorkflow(spec)` directly. Mirrors
   * `execute()`'s pre-run setup (policies via `applyKernelPolicies`, then `start_run`) and records a
   * best-effort `run_started` so the run is resumable. `runWorkflow` tears the kernel down afterward.
   */
  private async bootstrapWorkflowKernel(sessionId: string, spec: WorkflowSpec): Promise<KernelRuntimeHandle> {
    this.interrupted = false
    this.cancellationReason = undefined
    this.pendingObservations = []
    this.pendingPageOutArchives = []
    this.activePageOutArchive = undefined
    this.currentSessionId = sessionId

    const kernel = await getKernel()
    const runtime = new kernel.KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: this.opts.maxTurns ?? 25,
      ...(this.opts.maxTotalTokens !== undefined ? { maxTotalTokens: this.opts.maxTotalTokens } : {}),
      timeoutMs: this.opts.timeoutMs !== undefined ? BigInt(this.opts.timeoutMs) : undefined,
    })
    this.activeKernel = runtime
    const goal = `workflow:${spec.nodes.length} nodes`

    void Promise.resolve(
      this.opts.sessionLog.append(sessionId, {
        kind: "run_started",
        run_id: crypto.randomUUID(),
        goal,
        criteria: [],
        ...(this.opts.agentId ? { agent_id: this.opts.agentId } : {}),
      }),
    ).catch(() => {})

    this.applyKernelPolicies(runtime)
    kernelAction(runtime, this.pendingObservations, {
      kind: "start_run",
      task: { goal: `workflow session ${sessionId}`, criteria: [] },
    })
    return runtime
  }

  async runWorkflow(
    spec: WorkflowSpec,
    opts?: {
      resumedCompleted?: string[]
      /** W-1: recovered completions WITH control signals (classify branch / loop stop) — lowered to
       *  the kernel's `resumed_results` so control flow replays faithfully. Supersedes
       *  `resumedCompleted` for ids present in both. */
      resumedResults?: RecoveredNodeCompletion[]
      resumedSubmissions?: Record<string, unknown>[][]
      /** R3-1: original base index per submission batch (parallel to resumedSubmissions). */
      resumedSubmissionBases?: number[]
      /** W-1: recovered node outputs (agent id → output text) to pre-seed the driver's outputs map. */
      resumedOutputs?: Map<string, string>
      /** Standalone session id when bootstrapping (no active parent run). Defaults to a fresh uuid. */
      sessionId?: string
    },
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    // Standalone entry: with no active parent run, auto-bootstrap a kernel that owns the DAG (same
    // governance/quota policies a full run gets), drive it, then tear it down so the runner is reusable.
    // Mid-run callers keep the original in-place behavior with no teardown.
    const bootstrapped = !this.activeKernel || !this.currentSessionId
    if (bootstrapped) {
      await this.bootstrapWorkflowKernel(opts?.sessionId ?? `wf-${crypto.randomUUID()}`, spec)
    }
    const parentSessionId = this.currentSessionId!
    const runtime = this.activeKernel!

    try {
      const observationStart = this.pendingObservations.length
      const initialAction = kernelMaybeAction(runtime, this.pendingObservations, {
        kind: "load_workflow",
        spec: workflowSpecToKernel(spec),
        parent_session_id: parentSessionId,
        // W0-ABI resume: skip nodes already completed before an interruption.
        ...(opts?.resumedCompleted && opts.resumedCompleted.length ? { resumed_completed: opts.resumedCompleted } : {}),
        // W-1: signal-carrying completion records (classify branch / loop stop replay).
        ...(opts?.resumedResults?.length
          ? {
              resumed_results: opts.resumedResults.map(r => ({
                agent_id: r.agentId,
                ...(r.classifyBranch !== undefined ? { classify_branch: r.classifyBranch } : {}),
                ...(r.tournamentWinner !== undefined ? { tournament_winner: r.tournamentWinner } : {}),
                ...(r.loopContinue !== undefined ? { loop_continue: r.loopContinue } : {}),
              })),
            }
          : {}),
        // R3-1: re-apply recorded runtime submissions so dynamically-appended nodes are reconstructed.
        ...(opts?.resumedSubmissions && opts.resumedSubmissions.length ? { resumed_submissions: opts.resumedSubmissions } : {}),
        ...(opts?.resumedSubmissionBases && opts.resumedSubmissionBases.length ? { resumed_submission_bases: opts.resumedSubmissionBases } : {}),
      })
      const observations = this.pendingObservations.slice(observationStart)
      return await this.driveWorkflow(initialAction, observations, parentSessionId, runtime, opts?.resumedOutputs)
    } finally {
      if (bootstrapped) {
        this.activeKernel = null
        this.currentSessionId = null
        this.pendingObservations = []
      }
    }
  }

  /**
   * M5/G1: bootstrap an **agent-authored** workflow ("the model writes its own harness"). Routes the
   * spec through the agent-reachable `Syscall::LoadWorkflow` (`submit_workflow`): with no workflow
   * active the kernel bootstraps the DAG, else it flattens onto the running one (bootstrap-or-flatten —
   * one kernel, one quota). The same shared driver runs the resulting batches.
   */
  async bootstrapWorkflow(
    spec: WorkflowSpec,
    opts?: { submitterAgentId?: string },
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    if (!this.activeKernel || !this.currentSessionId) {
      throw new Error("bootstrapWorkflow requires an active parent run")
    }
    const parentSessionId = this.currentSessionId
    const runtime = this.activeKernel
    const observationStart = this.pendingObservations.length
    const initialAction = kernelMaybeAction(
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
   * M5 v2.1: drive the sub-workflow(s) a top-level agent authored via `start_workflow`, at the safe
   * point (tool turn resolved → kernel in Reason). Each runs in THIS kernel (the kernel resumes the
   * reason loop on `workflow_completed`), then the outcome is injected as a user message and a fresh
   * `call_provider` is synthesized from the updated context (the workflow drive consumed its own
   * kernel actions — same re-render pattern as the reactive-compact retry path).
   */
  private async driveAuthoredWorkflows(
    runtime: KernelRuntimeHandle,
    action: Extract<KernelRunnerAction, { kind: "call_provider" }>,
  ): Promise<Extract<KernelRunnerAction, { kind: "call_provider" }>> {
    const specs = this.pendingAuthoredWorkflows
    this.pendingAuthoredWorkflows = []
    this.workflowContinuation = null
    for (const spec of specs) {
      const outcome = await this.bootstrapWorkflow(spec)
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: messageToKernelMessage({ role: "user", content: authoredWorkflowOutcomeNote(outcome) }),
      })
    }
    const continuation = this.workflowContinuation as Extract<KernelRunnerAction, { kind: "call_provider" }> | null
    if (!continuation) {
      throw new Error("authored workflow completed without a provider continuation")
    }
    return {
      kind: "call_provider",
      effectId: continuation.effectId,
      context: runtime.render(),
      tools: continuation.tools,
    }
  }

  /**
   * #2-B-ii: while a workflow batch is in flight, poll the signal source; a Critical `InterruptNow`
   * routes through the kernel (root in `SubAgentAwait` → preempt → `AgentPreempted` + tears the
   * `WorkflowRun` down), and we abort the matching children's in-flight LLM calls. Returns the
   * torn-down outcome on preemption, else `null`. No-op without a signal source.
   */
  private async monitorWorkflowPreemption(
    runtime: KernelRuntimeHandle,
    controllers: Map<string, AbortController>,
    batchState: { settled: boolean },
  ): Promise<{ completed: string[]; failed: string[] } | null> {
    const source = this.opts.signalSource
    if (!source && this.injectedSignals.length === 0) return null
    while (!batchState.settled) {
      // O2: injected notes participate in the monitor too (drain order matches nextInboundSignal).
      const delivery = await this.nextInboundSignal()
      if (batchState.settled) break
      if (!delivery) { await new Promise(resolve => setTimeout(resolve, 5)); continue }
      const observationStart = this.pendingObservations.length
      const signalAction = await this.consumeInboundSignal(delivery, claimed =>
        kernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(claimed)))
      if (signalAction) {
        if (signalAction.kind !== "preempt_sub_agents") {
          throw new Error(`workflow signal returned unexpected effect: ${signalAction.kind}`)
        }
        for (const id of signalAction.agentIds) controllers.get(id)?.abort()
        const continuation = kernelMaybeAction(runtime, this.pendingObservations, {
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
        const wc = obs.find(o => o.kind === "workflow_completed") as { completed?: string[]; failed?: string[] } | undefined
        return { completed: wc?.completed ?? [], failed: wc?.failed ?? [] }
      }
    }
    return null
  }

  /**
   * Shared workflow driver for `runWorkflow` (host `load_workflow`) and `bootstrapWorkflow` (agent
   * `submit_workflow`): run each kernel-emitted batch in parallel, feed completions back (appending any
   * agent-submitted nodes first), and loop until the kernel reports the workflow complete.
   */
  private async driveWorkflow(
    initialAction: KernelRunnerAction | null,
    initial: KernelObservation[],
    parentSessionId: string,
    runtime: KernelRuntimeHandle,
    seedOutputs?: Map<string, string>,
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    const observations = initial
    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator

    const findDone = (obs: typeof observations) =>
      obs.find(o => o.kind === "workflow_completed") as { completed?: string[]; failed?: string[] } | undefined

    const acceptSpawn = (spawn: Extract<KernelRunnerAction, { kind: "spawn_workflow" }>): KernelObservation[] => {
      const observationStart = this.pendingObservations.length
      const continuation = kernelMaybeAction(runtime, this.pendingObservations, {
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
      return { completed: done.completed ?? [], failed: done.failed ?? [], outputs: {} }
    }
    if (!initialAction) return { completed: [], failed: [], outputs: {} }
    if (initialAction.kind !== "spawn_workflow") {
      throw new Error(`workflow load returned unexpected kernel effect: ${initialAction.kind}`)
    }
    let nodes = initialAction.nodes as unknown as WorkflowSpawnInfo[]
    let budget = initialAction.budget as unknown as WorkflowBudget | undefined
    acceptSpawn(initialAction)
    // G2: each completed node's output, keyed by agent id — a reduce node reads its deps' outputs.
    // W-1: on resume it is pre-seeded from the persisted node outputs, so post-resume dependents
    // still see their (pre-crash) dependencies' outputs.
    const outputs = new Map<string, string>(seedOutputs ?? [])

    for (;;) {
      if (nodes.length === 0) return { completed: [], failed: [], outputs: Object.fromEntries(outputs) }

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
      if (preempted) return { ...preempted, outputs: Object.fromEntries(outputs) }

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
        // R3-1: if this node's agent submitted more nodes, append them to the parent DAG BEFORE
        // reporting the node's completion — the workflow is still active, so even a last-node
        // submission keeps the DAG alive.
        if (result.submittedNodes?.length) {
          // G1: stamp the submitting node's agent id so the kernel coerces a quarantined submitter's
          // nodes to quarantined (no topological privilege escalation).
          const submitEvent = submitWorkflowNodesToKernel(result.submittedNodes, result.agentId)
          const observationStart = this.pendingObservations.length
          const submitAction = kernelMaybeAction(runtime, this.pendingObservations, submitEvent)
          const subObs = this.pendingObservations.slice(observationStart)
          if (submitAction?.kind === "spawn_workflow") {
            nextNodes.push(...submitAction.nodes as unknown as WorkflowSpawnInfo[])
            budget = submitAction.budget as unknown as WorkflowBudget | undefined ?? budget
            acceptSpawn(submitAction)
          } else if (submitAction) {
            throw new Error(`workflow node submission returned unexpected effect: ${submitAction.kind}`)
          }
          // R3-1: persist the submission (kernel-shape nodes) + its kernel-reported base index
          // so resume can re-apply the batch at the exact original graph position. W-N3: also the
          // submitter, so resume drops batches whose submitter re-runs (it will re-submit).
          const submitted = subObs.find(o => o.kind === "workflow_nodes_submitted") as
            | { base?: number }
            | undefined
          await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
            turn: runtime.turn(),
            nodes: (submitEvent.nodes as Record<string, unknown>[]) ?? [],
            baseIndex: submitted?.base,
            submitterAgentId: result.agentId,
          }))
        }
        const observationStart = this.pendingObservations.length
        const completionAction = kernelMaybeAction(runtime, this.pendingObservations, {
          kind: "sub_agent_completed",
          result: subAgentResultToKernel(result),
        })
        let obs = this.pendingObservations.slice(observationStart)
        if (completionAction?.kind === "spawn_workflow") {
          nextNodes.push(...completionAction.nodes as unknown as WorkflowSpawnInfo[])
          budget = completionAction.budget as unknown as WorkflowBudget | undefined ?? budget
          obs = [...obs, ...acceptSpawn(completionAction)]
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
          termination: result.result.termination,
          classifyBranch: result.result.classifyBranch,
          tournamentWinner: result.result.tournamentWinner,
          loopContinue: result.result.loopContinue,
          ...(result.result.termination === "completed" && outText ? { output: outText } : {}),
        }))
      }
      if (done && nextNodes.length === 0) {
        return { completed: done.completed ?? [], failed: done.failed ?? [], outputs: Object.fromEntries(outputs) }
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
  ): Promise<{ completed: string[]; failed: string[] }> {
    // Standalone resume: a stateless handler passes the prior `sessionId`; mid-run callers omit it.
    const sessionId = opts?.sessionId ?? this.currentSessionId
    if (!sessionId) {
      throw new Error("resumeWorkflow requires an active parent run or an explicit sessionId")
    }
    const events = await this.opts.sessionLog.read(sessionId)
    const resumedResults = recoverCompletedWorkflowNodes(events)
    const completedIds = new Set(resumedResults.map(r => r.agentId))
    const recovered = recoverSubmittedWorkflowNodes(events)
    // W-N3: DROP batches whose submitter did NOT complete — that node re-runs on resume and will
    // re-submit its batch; replaying the logged copy too would duplicate its nodes in the DAG.
    // Only safe with exact bases (the dropped batch's slots become inert placeholders); a legacy
    // order-only log keeps every batch, since dropping would shift all later indices.
    let { submissions, bases } = recovered
    if (bases.length === submissions.length && submissions.length > 0) {
      const keep = recovered.submitters.map(s => s === undefined || completedIds.has(s))
      submissions = submissions.filter((_, i) => keep[i])
      bases = bases.filter((_, i) => keep[i])
    }
    const resumedOutputs = new Map(
      resumedResults.filter(r => r.output).map(r => [r.agentId, r.output as string]),
    )
    // Alias loop iterations onto their stable node id (last iteration wins) — dependents consume
    // `wf-node{N}`, not `wf-node{N}-i{k}`.
    for (const r of resumedResults) {
      const stableId = r.agentId.replace(/-i\d+$/, "")
      if (stableId !== r.agentId && r.output) resumedOutputs.set(stableId, r.output)
    }
    return this.runWorkflow(spec, {
      resumedResults,
      resumedSubmissions: submissions,
      resumedSubmissionBases: bases,
      resumedOutputs,
      sessionId,
    })
  }

  private async appendObservations(
    sessionId: string,
    runtime: KernelRuntimeHandle,
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
    runtime: KernelRuntimeHandle,
    phase: "initial" | "renewal",
  ): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId) return
    // P10: recall is default-on (CC session-start recall) — with no hook configured,
    // the goal itself is the query. preQueryMemory stays as the targeting override.
    const preQuery = this.opts.preQueryMemory
      ?? ((ctx: { goal: string }) => [ctx.goal])
    try {
      const queries = await preQuery({ goal: this.currentGoal, phase })
      const lines: string[] = []
      for (const q of queries ?? []) {
        if (typeof q !== "string" || !q.trim()) continue
        const hits = await this.opts.dreamStore.search(this.opts.agentId, q, 5)
        for (const hit of hits) {
          lines.push(`[memory score=${hit.score.toFixed(3)}] ${hit.text}`)
        }
      }
      if (lines.length > 0) {
        kernelApply(runtime, this.pendingObservations, {
          kind: "add_history_message",
          message: { role: "user", content: lines.join("\n") },
        })
      }
    } catch { /* errs-open */ }
  }

  private async archiveSemanticPageOut(archived: Message[], action?: string): Promise<void> {
    if (!this.opts.dreamStore || !this.opts.agentId) return
    try {
      const summary = this.opts.dreamSummarizer
        ? await this.opts.dreamSummarizer.summarize(archived, { action })
        : await summarizeForLongTermMemory(
          this.opts.dreamProvider ?? this.opts.provider,
          archived,
          this.opts.dreamSystemPrompt,
        )
      const existing = await this.opts.dreamStore.loadMemories(this.opts.agentId)
      // P2 write-funnel (wasm surface has no kernel write gate yet): jaccard dedup +
      // advisory 0.6 score — an automatic summary must never outrank curated content.
      if (existing.some(e => jaccardSimilarity(e.text, summary) >= 0.9)) return
      await this.opts.dreamStore.commit(this.opts.agentId, {
        toAdd: [{ text: summary, score: 0.6, metadata: { source: "semantic_page_out", action } }],
        toRemoveIndices: [],
        stats: {
          insightsProcessed: 1,
          duplicatesRemoved: 0,
          conflictsResolved: 0,
          entriesAdded: 1,
        },
      }, existing)
    } catch {
      // non-fatal
    }
  }
}

/** Word-set jaccard similarity — the curator's dedup rule at the write funnel. */
function jaccardSimilarity(a: string, b: string): number {
  const sa = new Set(a.split(/\s+/).filter(Boolean))
  const sb = new Set(b.split(/\s+/).filter(Boolean))
  if (sa.size === 0 && sb.size === 0) return 1
  let inter = 0
  for (const w of sa) if (sb.has(w)) inter++
  const union = sa.size + sb.size - inter
  return union === 0 ? 0 : inter / union
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
      messages.push({
        role: "user",
        content: userText,
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

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}

export async function collectText(stream: AsyncIterable<StreamEvent>): Promise<string> {
  let text = ""
  for await (const evt of stream) {
    if (evt.type === "text_delta") text += (evt as TextDelta).delta
  }
  return text
}

/** R3-1: parse `submit_workflow_nodes` tool args (`{ nodes: WorkflowNodeSpec[] }`). Node shapes are
 *  trusted structurally; the kernel validates them on append. Malformed payload → no nodes. */
function parseSubmitWorkflowNodesArgs(argsStr: string): WorkflowNodeSpec[] {
  let parsed: Record<string, unknown> = {}
  try {
    parsed = JSON.parse(argsStr) as Record<string, unknown>
  } catch {
    // Ignore parse error → no nodes submitted.
  }
  return Array.isArray(parsed.nodes) ? (parsed.nodes as WorkflowNodeSpec[]) : []
}

/** M5 v1: parse `start_workflow` tool args (`{ spec: { nodes: WorkflowNodeSpec[] } }`) into the
 *  spec's node batch — flattened onto the running workflow via the same append path. */
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

/** M5 v2.1: parse the full `WorkflowSpec` from a top-level `start_workflow` call for the auto-pivot
 *  drive. Returns `undefined` on a malformed / empty payload (caller falls back to the flatten path). */
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
 *  agent's context, so its next turn continues with the sub-workflow's results in view. */
function authoredWorkflowOutcomeNote(outcome: {
  completed: string[]
  failed: string[]
  outputs: Record<string, string>
}): string {
  const lines = [
    `[authored workflow result] ${outcome.completed.length} node(s) completed` +
      (outcome.failed.length ? `, ${outcome.failed.length} failed` : "") + ".",
  ]
  for (const id of outcome.completed) {
    const out = outcome.outputs[id]
    if (out) lines.push(`- ${id}: ${out.length > 500 ? out.slice(0, 500) + "…" : out}`)
  }
  return lines.join("\n")
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
      timestamp_ms: Date.now(),
    },
  }
}
