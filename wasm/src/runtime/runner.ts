import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, WorkflowNodesSubmittedEvent, DoneEvent, ErrorEvent,
  ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  DreamSummarizer,
} from "../types.js"
import type { ToolSuspendEvent } from "./execution-plane.js"
import type { DreamStore, DreamResult, MemoryEntry, CurationResult, SessionData } from "../memory/index.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import type { SignalSource, RuntimeSignal } from "../signals/index.js"
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
} from "./session-repair.js"
import {
  forceCompact,
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
  loopInstruction, classifyInstruction, judgeGoal,
  extractLoopContinue, extractClassifyBranch, extractJudgeWinner,
} from "./workflow-control-flow.js"
import { kernelObservationToSessionEvent, withCategory } from "./kernel-event-log.js"
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

export interface RuntimeOptions {
  provider: LLMProvider
  /** M1/G3 intelligence routing: resolve a per-node provider from a workflow node's `modelHint`.
   *  Returns undefined ⇒ fall back to `provider`. Without this hook the hint is a no-op. */
  providerFor?: (modelHint: string) => LLMProvider | undefined
  /** M4/G5: cumulative token cap for this run (the kernel's `max_total_tokens`); a node's `tokenBudget`
   *  flows here for its child run. Undefined ⇒ the kernel default. */
  maxTotalTokens?: number
  sessionLog: SessionLog
  executionPlane: ExecutionPlane
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  agentId?: string
  /** I4: optional run-start memory pre-fetch hook (mirrors Node SDK). Called once per run before
   *  the first LLM turn; each returned query string becomes a dreamStore search; hits page into
   *  the knowledge partition before turn 1. Requires dreamStore + agentId. */
  preQueryMemory?: (ctx: { goal: string }) => Promise<string[] | undefined> | string[] | undefined
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
  resourceQuota?: ResourceQuota
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

export class RuntimeRunner {
  private interrupted = false
  /** #2-B-ii: aborts the in-flight provider stream on interrupt/preempt. Recreated per `execute`. */
  private abortController: AbortController | null = null
  private pendingObservations: KernelObservation[] = []
  private activeKernel: KernelRuntimeHandle | null = null
  private currentSessionId: string | null = null
  private nextArchiveStart = 0
  private localPageOutCache: Message[] = []
  /** M5 v2.1: sub-workflow specs a top-level agent authored via `start_workflow`, awaiting auto-drive
   *  at the next safe point (after the tool turn resolves, kernel back in Reason). */
  private pendingAuthoredWorkflows: WorkflowSpec[] = []
  private pendingSpoolOutputs = new Map<string, { tool: string; output: string }>()

  constructor(private readonly opts: RuntimeOptions) {}

  get hostOptions(): RuntimeOptions { return this.opts }

  interrupt(): void { this.interrupted = true; this.abortController?.abort() }

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

  /** Push content into Slot 2 (system_knowledge) via add_knowledge_message. */
  pushKnowledge(message: Message, tokens?: number): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, {
      kind: "add_knowledge_message",
      content: message.content ?? "",
      tokens: tokens ?? Math.max(1, Math.ceil((message.content?.length ?? 0) / 4)),
    })
  }

  /** Phase 4: satisfy kernel page-in requests before meta-tool execution. */
  private async applyKernelPageIn(
    runtime: KernelRuntimeHandle,
    sessionId: string,
  ): Promise<void> {
    const requests = this.pendingObservations.filter(
      (o): o is KernelObservation & { kind: "page_in_requested"; tool: string; query: string; top_k?: number } =>
        o.kind === "page_in_requested" && typeof o.tool === "string",
    )
    if (requests.length === 0) return

    const entries: Array<{ content: string; tokens?: number; source?: string }> = []
    for (const req of requests) {
      const query = typeof req.query === "string" ? req.query : ""
      const topK = typeof req.top_k === "number" ? req.top_k : 5
      if (req.tool === "memory") {
        const localHits = this.localPageOutCache.filter(m =>
          typeof m.content === "string" && m.content.toLowerCase().includes(query.toLowerCase())
        ).slice(0, topK)

        for (const hit of localHits) {
          entries.push({
            content: `[local semantic cache] ${hit.role}: ${hit.content}`,
            source: "semantic_cache",
          })
        }

        const remainingK = topK - entries.length
        if (remainingK > 0 && this.opts.dreamStore && this.opts.agentId) {
          const hits = await this.opts.dreamStore.search(this.opts.agentId, query, remainingK)
          for (const hit of hits) {
            entries.push({
              content: `[memory score=${hit.score.toFixed(3)}] ${hit.text}`,
              source: "memory",
            })
          }
        }
      } else if (req.tool === "knowledge" && this.opts.knowledgeSource) {
        const snippets = await this.opts.knowledgeSource.retrieve(query, topK)
        for (const snippet of snippets) {
          entries.push({ content: snippet, source: "knowledge" })
        }
      }
    }
    if (entries.length === 0) return
    kernelApply(runtime, this.pendingObservations, { kind: "page_in", entries })
    await this.opts.sessionLog.append(sessionId, withCategory({
      kind: "page_in",
      turn: runtime.turn(),
      entry_count: entries.length,
    }))
  }

  private async resolveKernelSuspend(
    runtime: KernelRuntimeHandle,
    sessionId: string,
  ): Promise<{ approved: string[]; denied: string[]; events: StreamEvent[] }> {
    const gated = this.pendingObservations.filter(
      (o): o is KernelObservation & { kind: "tool_gated"; call_id: string; tool: string; reason: string } =>
        o.kind === "tool_gated" && typeof o.call_id === "string" && typeof o.tool === "string",
    )
    const approved: string[] = []
    const denied: string[] = []
    const events: StreamEvent[] = []
    const runCtx: RunContext = { onPermissionRequest: this.opts.onPermissionRequest }

    for (const g of gated) {
      const request: PermissionRequestEvent = {
        type: "permission_request",
        callId: g.call_id,
        toolName: g.tool,
        arguments: "{}",
        reason: typeof g.reason === "string" ? g.reason : "",
      }
      events.push(request)
      const decision = await resolvePermissionRequest(request, runCtx)
      events.push({
        type: "permission_resolved",
        callId: g.call_id,
        toolName: g.tool,
        approved: decision.approved,
        responder: decision.responder ?? "host",
        ...(decision.reason ? { reason: decision.reason } : {}),
      } as PermissionResolvedEvent)
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_requested",
        turn: runtime.turn(),
        tool: g.tool,
        arguments: "{}",
        reason: request.reason,
      })
      await this.opts.sessionLog.append(sessionId, {
        kind: "permission_resolved",
        turn: runtime.turn(),
        approved: decision.approved,
        responder: decision.responder ?? "host",
      })
      if (decision.approved) {
        approved.push(g.call_id)
      } else {
        denied.push(g.call_id)
        const denyReason = decision.reason ?? "permission denied"
        events.push({
          type: "tool_denied",
          callId: g.call_id,
          toolName: g.tool,
          reason: denyReason,
        } as ToolDeniedEvent)
        events.push({
          type: "tool_result",
          callId: g.call_id,
          name: g.tool,
          content: `permission denied: ${denyReason}`,
          isError: true,
          errorKind: "governance_denied",
        } as ToolResultEvent)
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_denied",
          turn: runtime.turn(),
          call_id: g.call_id,
          tool_name: g.tool,
          reason: denyReason,
        })
        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: runtime.turn(),
          results: [{
            call_id: g.call_id,
            output: `permission denied: ${denyReason}`,
            is_error: true,
            error_kind: "governance_denied",
          }],
        })
      }
    }

    return { approved, denied, events }
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
    this.abortController = new AbortController()
    this.pendingObservations = []
    this.pendingSpoolOutputs.clear()
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
      const replayed = replayMessages(repaired, maxBytes)
      kernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayed.map(messageToKernelMessage),
      })
      // P1-B B3: rebuild active-skill gating after a wake (active_skills is not snapshotted).
      for (const m of replayed) {
        for (const tc of m.toolCalls ?? []) {
          if (tc.name !== "skill") continue
          try {
            const name = (JSON.parse(tc.arguments || "{}") as { name?: string }).name
            if (name) kernelApply(runtime, this.pendingObservations, { kind: "skill_activated", name })
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

    // I4: pre-fetch memory into the knowledge partition before the first LLM turn (mirrors Node).
    if (!resumeMidRun && this.opts.preQueryMemory && this.opts.dreamStore && this.opts.agentId) {
      try {
        const queries = await this.opts.preQueryMemory({ goal })
        const entries: Array<{ content: string; tokens?: number; source?: string }> = []
        for (const q of queries ?? []) {
          if (typeof q !== "string" || !q.trim()) continue
          const hits = await this.opts.dreamStore.search(this.opts.agentId, q, 5)
          for (const hit of hits) {
            entries.push({ content: `[memory score=${hit.score.toFixed(3)}] ${hit.text}`, source: "memory" })
          }
        }
        if (entries.length > 0) {
          kernelApply(runtime, this.pendingObservations, { kind: "page_in", entries })
        }
      } catch { /* errs-open */ }
    }

    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    let hasAttemptedReactiveCompact = false
    // P0-C: the skill loaded and in effect going into the current turn → per-turn `activeSkill` metric.
    let activeSkill: string | undefined

    // I0b: kernel-throw safety net — see Node runner for full rationale.
    try {
    while (!runtime.isTerminal()) {
      if (action.kind === "execute_tool") {
        await this.applyKernelPageIn(runtime, sessionId)
      }
      nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
      if (this.interrupted) {
        action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
        break
      }

      if (this.opts.signalSource) {
        const sig = await this.opts.signalSource.nextSignal()
        if (sig) {
          const sigAction = kernelMaybeAction(runtime, this.pendingObservations, signalToKernelEvent(sig))
          if (sigAction) action = sigAction
          // I0a: Critical signal carries user_abort intent; see Node runner for full rationale.
          if (sig.urgency === "critical") this.interrupted = true
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
        let shouldRetry = false

        const abortSignal = this.abortController?.signal
        try {
          for await (const evt of this.opts.provider.stream(context, tools, Object.keys(ext).length ? ext : undefined, providerState, abortSignal)) {
            // #2-B-ii: a preempting interrupt fires abortController — stop consuming the live stream.
            if (abortSignal?.aborted) break
            if (evt.type === "usage") {
              const usageEvt = evt as { type: string; totalTokens: number; inputTokens?: number; outputTokens?: number; cacheReadInputTokens?: number; cacheCreationInputTokens?: number; cacheReadInputTokensBySlot?: { system?: number; tools?: number; messages?: number } }
              turnTokens = usageEvt.totalTokens
              turnInputTokens = usageEvt.inputTokens ?? 0
              turnOutputTokens = usageEvt.outputTokens ?? 0
              // P0-C: capture the prompt-cache split for the tool-gating hit-rate baseline.
              turnCacheReadTokens = usageEvt.cacheReadInputTokens ?? 0
              turnCacheCreationTokens = usageEvt.cacheCreationInputTokens ?? 0
              // I1: per-slot attribution forwarded to TurnMetrics; undefined on non-Anthropic providers.
              turnCacheReadBySlot = usageEvt.cacheReadInputTokensBySlot
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
          // #2-B-ii: an aborted in-flight request surfaces as an AbortError — treat as an interrupt.
          if (abortSignal?.aborted) { this.interrupted = true }
          const errMsg = formatToolError(err).toLowerCase()
          if (
            (errMsg.includes("413") || errMsg.includes("too long") || errMsg.includes("context length exceeded") || errMsg.includes("context_length_exceeded")) &&
            !hasAttemptedReactiveCompact
          ) {
            hasAttemptedReactiveCompact = true
            if (forceCompact(runtime, this.pendingObservations)) {
              nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
              shouldRetry = true
            }
          }
          if (!shouldRetry) {
            yield { type: "error", message: formatToolError(err) } as ErrorEvent
            action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
            break
          }
        }

        // #2-B-ii: stream aborted (preempt/interrupt) via the break path — end the turn now.
        if (abortSignal?.aborted) {
          action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
          break
        }

        if (shouldRetry) {
          action = {
            kind: "call_provider",
            context: runtime.render(),
            tools,
          }
          continue
        }

        const assistantMessage: Message = {
          role: "assistant",
          content: finalText,
          toolCalls: finalToolCalls,
          tokenCount: turnOutputTokens || turnTokens || undefined,
        }
        const providerEvent: Record<string, unknown> = {
          kind: "provider_result",
          message: messageToKernelMessage(assistantMessage),
          ...(turnInputTokens > 0 ? { observed_input_tokens: turnInputTokens } : {}),
          ...(turnOutputTokens > 0 ? { observed_output_tokens: turnOutputTokens } : {}),
          now_ms: Date.now(),
        }
        let nextAction = kernelMaybeAction(runtime, this.pendingObservations, providerEvent)
        const hasSuspended = this.pendingObservations.some(o => o.kind === "suspended")
        if (!nextAction && hasSuspended) {
          const resolved = await this.resolveKernelSuspend(runtime, sessionId)
          for (const evt of resolved.events) yield evt
          nextAction = kernelAction(runtime, this.pendingObservations, {
            kind: "resume",
            approved_calls: resolved.approved,
            denied_calls: resolved.denied,
          })
        }
        action = nextAction ?? kernelAction(runtime, this.pendingObservations, providerEvent)
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

      } else if (action.kind === "execute_tool") {
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
        const normalCalls = allCalls.filter(c => c.name !== "submit_workflow_nodes" && c.name !== "start_workflow")
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
        for await (const evt of this.opts.executionPlane.executeAll(normalCalls, runCtx)) {
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
        for (const call of allCalls) {
          const result = toolResults.find(r => r.callId === call.id)
          if (result) {
            this.pendingSpoolOutputs.set(call.id, { tool: call.name, output: result.output })
          }
        }
        // P1-B B3: a successfully-resolved `skill` call activates that skill for the next turn.
        for (const call of allCalls) {
          if (call.name !== "skill") continue
          const res = toolResults.find(r => r.callId === call.id)
          if (!res || res.isError) continue
          try {
            const name = (JSON.parse(call.arguments || "{}") as { name?: string }).name
            if (name) kernelApply(runtime, this.pendingObservations, { kind: "skill_activated", name })
          } catch { /* skip */ }
        }
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "tool_results",
          results: toolResults.map(toolResultToKernel),
        })

      } else if (action.kind === "evaluate_milestone") {
        const milestonePolicy = this.opts.milestonePolicy ?? "require_verifier"
        if (milestonePolicy === "auto_pass") {
          action = kernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
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
    const status = result?.termination ?? (this.interrupted ? "user_abort" : "error")
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

    yield { type: "done", iterations: turnsUsed, totalTokens, status } as DoneEvent
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
    const withBudget = (goal: string) => (budgetNote ? `${goal}\n\n${budgetNote}` : goal)
    const mkCtx = (goal: string) => ({
      parentOpts: this.opts,
      parentSessionId,
      spec: { ...baseSpec, goal: withBudget(goal) },
      manifest,
      sessionLog: this.opts.sessionLog,
      // M5 v2.1: this child IS a workflow node — its `start_workflow` flattens to this kernel.
      isWorkflowNode: true,
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

    // A#2 v2 loop iteration: run the increment, then extract a stop signal. No signal ⇒ run to cap.
    if (node.loop_max_iters != null) {
      const result = await orchestrator.run(mkCtx(`${baseSpec.goal}\n\n${loopInstruction(node.loop_max_iters)}`))
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

    kernelApply(runtime, this.pendingObservations, governancePolicyToKernelEvent(governancePolicy))
    kernelApply(runtime, this.pendingObservations, {
      kind: "set_attention_policy",
      ...(attentionPolicy.maxQueueSize !== undefined
        ? { max_queue_size: attentionPolicy.maxQueueSize }
        : {}),
    })
    if (this.opts.schedulerBudget) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_scheduler_budget",
        ...(this.opts.schedulerBudget.maxWallMs !== undefined
          ? { max_wall_ms: this.opts.schedulerBudget.maxWallMs }
          : {}),
      })
    }
    if (this.opts.resourceQuota) {
      const q = this.opts.resourceQuota
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_resource_quota",
        quota: {
          ...(q.maxConcurrentSubagents !== undefined
            ? { max_concurrent_subagents: q.maxConcurrentSubagents }
            : {}),
          ...(q.maxSpawnDepth !== undefined ? { max_spawn_depth: q.maxSpawnDepth } : {}),
          ...(q.memoryWritesPerWindow !== undefined
            ? {
                memory_writes_per_window: [
                  q.memoryWritesPerWindow.maxWrites,
                  q.memoryWritesPerWindow.windowMs,
                ],
              }
            : {}),
        },
      })
    }
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
    this.pendingObservations = []
    this.pendingSpoolOutputs.clear()
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
    kernelApply(runtime, this.pendingObservations, { kind: "start_run", task: { goal, criteria: [] } })
    return runtime
  }

  async runWorkflow(
    spec: WorkflowSpec,
    opts?: {
      resumedCompleted?: string[]
      resumedSubmissions?: Record<string, unknown>[][]
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
      const observations = kernelApply(runtime, this.pendingObservations, {
        kind: "load_workflow",
        spec: workflowSpecToKernel(spec),
        parent_session_id: parentSessionId,
        // W0-ABI resume: skip nodes already completed before an interruption.
        ...(opts?.resumedCompleted && opts.resumedCompleted.length ? { resumed_completed: opts.resumedCompleted } : {}),
        // R3-1: re-apply recorded runtime submissions so dynamically-appended nodes are reconstructed.
        ...(opts?.resumedSubmissions && opts.resumedSubmissions.length ? { resumed_submissions: opts.resumedSubmissions } : {}),
      })
      return await this.driveWorkflow(observations, parentSessionId, runtime)
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
    const observations = kernelApply(
      runtime,
      this.pendingObservations,
      submitWorkflowToKernel(spec, parentSessionId, opts?.submitterAgentId),
    )
    return this.driveWorkflow(observations, parentSessionId, runtime)
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
    for (const spec of specs) {
      const outcome = await this.bootstrapWorkflow(spec)
      kernelApply(runtime, this.pendingObservations, {
        kind: "add_history_message",
        message: messageToKernelMessage({ role: "user", content: authoredWorkflowOutcomeNote(outcome) }),
      })
    }
    return { kind: "call_provider", context: runtime.render(), tools: action.tools }
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
    if (!source) return null
    while (!batchState.settled) {
      const sig = await source.nextSignal()
      if (batchState.settled) break
      if (!sig) { await new Promise(resolve => setTimeout(resolve, 5)); continue }
      const obs = kernelApply(runtime, this.pendingObservations, signalToKernelEvent(sig))
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
    initial: KernelObservation[],
    parentSessionId: string,
    runtime: KernelRuntimeHandle,
  ): Promise<{ completed: string[]; failed: string[]; outputs: Record<string, string> }> {
    const observations = initial
    const orchestrator = this.opts.subAgentOrchestrator ?? defaultSubAgentOrchestrator

    const collectNodes = (obs: typeof observations): WorkflowSpawnInfo[] =>
      (obs.find(o => o.kind === "workflow_batch_spawned") as { nodes?: WorkflowSpawnInfo[] } | undefined)?.nodes ?? []
    // G4: the batch observation carries the workflow's remaining budget; track the latest.
    const collectBudget = (obs: typeof observations): WorkflowBudget | undefined =>
      (obs.find(o => o.kind === "workflow_batch_spawned") as { budget?: WorkflowBudget } | undefined)?.budget
    const findDone = (obs: typeof observations) =>
      obs.find(o => o.kind === "workflow_completed") as { completed?: string[]; failed?: string[] } | undefined

    let done = findDone(observations)
    if (done) return { completed: done.completed ?? [], failed: done.failed ?? [], outputs: {} }
    let nodes = collectNodes(observations)
    let budget = collectBudget(observations)
    // G2: each completed node's output, keyed by agent id — a reduce node reads its deps' outputs.
    const outputs = new Map<string, string>()

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
        outputs.set(result.agentId, typeof outContent === "string" ? outContent : outContent != null ? JSON.stringify(outContent) : "")
        // R3-1: if this node's agent submitted more nodes, append them to the parent DAG BEFORE
        // reporting the node's completion — the workflow is still active, so even a last-node
        // submission keeps the DAG alive.
        if (result.submittedNodes?.length) {
          // G1: stamp the submitting node's agent id so the kernel coerces a quarantined submitter's
          // nodes to quarantined (no topological privilege escalation).
          const submitEvent = submitWorkflowNodesToKernel(result.submittedNodes, result.agentId)
          const subObs = kernelApply(runtime, this.pendingObservations, submitEvent)
          nextNodes.push(...collectNodes(subObs))
          budget = collectBudget(subObs) ?? budget
          // R3-1: persist the submission (kernel-shape nodes) so resume can re-apply it.
          await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodesSubmittedEvent({
            turn: runtime.turn(),
            nodes: (submitEvent.nodes as Record<string, unknown>[]) ?? [],
          }))
        }
        const obs = kernelApply(runtime, this.pendingObservations, {
          kind: "sub_agent_completed",
          result: subAgentResultToKernel(result),
        })
        nextNodes.push(...collectNodes(obs))
        budget = collectBudget(obs) ?? budget
        const d = findDone(obs)
        if (d) done = d
        // Persist node completion for resume recovery.
        await this.opts.sessionLog.append(parentSessionId, buildWorkflowNodeCompletedEvent({
          turn: runtime.turn(),
          agentId: result.agentId,
          termination: result.result.termination,
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
   * Reads the session log, extracts completed workflow node agent_ids, and
   * calls runWorkflow with resumedCompleted so the kernel skips those nodes.
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
    const resumedCompleted = recoverCompletedWorkflowNodes(events)
    const resumedSubmissions = recoverSubmittedWorkflowNodes(events)
    return this.runWorkflow(spec, { resumedCompleted, resumedSubmissions, sessionId })
  }

  private async appendObservations(
    sessionId: string,
    runtime: KernelRuntimeHandle,
    nextArchiveStart: number,
  ): Promise<number> {
    const turn = runtime.turn()
    const preservedRefs = runtime.preservedRefs()
    const observations = this.pendingObservations.splice(0)
    for (let obs of observations) {
      if (obs.kind === "page_in_requested") continue

      let spoolRef: string | undefined
      if (obs.kind === "large_result_spooled") {
        const pending = this.pendingSpoolOutputs.get(obs.call_id ?? "")
        if (pending) {
          const spool = this.opts.resultSpool ?? new LargeResultSpool()
          try {
            spoolRef = await spool.persistOutput(obs.call_id ?? "", pending.output)
          } catch {
            // non-fatal
          }
          if (!obs.tool && pending.tool) {
            obs = { ...obs, tool: pending.tool }
          }
          this.pendingSpoolOutputs.delete(obs.call_id ?? "")
        }
      }

      const latest =
        obs.kind === "compressed" ? await this.opts.sessionLog.latestSeq(sessionId) : undefined
      const event = kernelObservationToSessionEvent(obs, turn, {
        nextArchiveStart,
        latestSeq: latest,
        preservedRefs,
        spoolRef,
        compressionAction,
      })
      if (!event) continue

      if (obs.kind === "page_out" && obs.archived) {
        this.localPageOutCache.push(...(obs.archived as Message[]))
      }

      const compressedSeq = await this.opts.sessionLog.append(sessionId, event)
      if (event.kind === "compressed") {
        nextArchiveStart = compressedSeq + 1
      }
      if (
        obs.kind === "page_out"
        && obs.tier_hint === "semantic"
        && Array.isArray(obs.archived)
        && obs.archived.length > 0
      ) {
        void this.archiveSemanticPageOut(obs.archived as Message[], compressionAction(obs.action))
      }
    }
    return nextArchiveStart
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
      await this.opts.dreamStore.commit(this.opts.agentId, {
        toAdd: [{ text: summary, score: 1.0, metadata: { source: "semantic_page_out", action } }],
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

function replayMessages(events: Array<{ seq: number; event: SessionEvent }>, maxBytes?: number): Message[] {
  // Build upgraded-summary index: compressed_seq -> upgraded summary
  const upgradedSummaries = new Map<number, string>()
  for (const { event: e } of events) {
    if (e.kind === "summary_upgraded") upgradedSummaries.set(e.compressed_seq, e.summary)
  }

  const messages: Message[] = []
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

/** Lower a host `RuntimeSignal` to the kernel's snake_case `signal` input event. Shared by the main
 *  loop's per-turn poll and #2-B-ii's workflow-batch preemption monitor (so the two never drift). */
function signalToKernelEvent(sig: RuntimeSignal): Record<string, unknown> {
  return {
    kind: "signal",
    signal: {
      id: crypto.randomUUID(),
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
