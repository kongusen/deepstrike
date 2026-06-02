import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  ToolSuspendEvent, ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent,
  PermissionResponse, PermissionResolvedEvent, AsyncSummarizer,
} from "../types.js"
import type { DreamStore, MemoryEntry, CurationResult, SessionData } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import type { SignalSource, RuntimeSignal } from "../signals/types.js"
import type { SessionLog, SessionEvent, RollbackReason } from "./session-log.js"
import type { ArchiveStore } from "./archive.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { getKernel, type KernelRuntimeInstance } from "../kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import { buildLlmCompletedEvent, buildRunTerminalEvent, repairEventsForRecovery } from "./session-repair.js"
import {
  capabilityMarker,
  capabilitySkill,
  capabilityTool,
  capabilityCommandMount,
  capabilityCommandUnmount,
  kernelAction,
  kernelApply,
  kernelMaybeAction,
  forceCompact,
  messageToKernelMessage,
  skillMetadataToKernel,
  taskUpdateToKernel,
  toolResultToKernel,
  toolSchemaToKernel,
  type KernelObservation,
  type KernelRunnerAction,
} from "./kernel-step.js"
import type {
  AgentRunSpec, AgentSpawnedObservation, MilestoneCheckResult, MilestoneContract, MilestonePolicy, SubAgentResult,
} from "../types/agent.js"
import {
  agentRunSpecToKernel,
  findSpawnProcessObservation,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  spawnObservationToManifest,
  subAgentResultToKernel,
} from "../types/agent.js"
import { defaultSubAgentOrchestrator, type SubAgentOrchestrator } from "./sub-agent-orchestrator.js"
import { governancePolicyToKernelEvent, type GovernancePolicy } from "../governance.js"
import { kernelObservationToSessionEvent, withCategory } from "./kernel-event-log.js"
import { assertNativeProfile, isNativeProfile, type OsProfile } from "./os-profile.js"

export interface RuntimeOptions {
  /**
   * SDK runtime profile. `native` enables Phase 6 OS contract: in-kernel signal +
   * governance only, mandatory event categories on kernel session events.
   * @default "legacy"
   */
  osProfile?: OsProfile
  provider: LLMProvider
  sessionLog: SessionLog
  executionPlane: ExecutionPlane
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  agentId?: string
  systemPrompt?: string
  initialMemory?: string[]
  skillDir?: string
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  // COMPAT(gov-sdk-gate): legacy SDK-side governance instance. Prefer the declarative
  // `governancePolicy` below, which drives the in-kernel gate. When both are set,
  // `governancePolicy` wins and this instance is ignored. Removable after migration.
  governance?: {
    setTime?(nowMs: bigint): void
    evaluate(name: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
  }
  /**
   * Declarative governance policy loaded into the kernel (`load_governance_policy`).
   * The kernel enforces deny/veto/rate-limit/param-constraint before tools execute;
   * AskUser calls surface as `tool_gated` and run through `onPermissionRequest`.
   */
  governancePolicy?: GovernancePolicy
  /**
   * Enable in-kernel signal routing (`set_attention_policy`). When set, inbound
   * signals are dispatched through the kernel attention policy (dedup + disposition
   * + queue) and surface as `signal_disposed` observations, instead of the legacy
   * SDK-side router. `maxQueueSize` defaults to 64.
   */
  attentionPolicy?: { maxQueueSize?: number }
  /**
   * Optional scheduler budget overrides. `maxWallMs` is the wall-clock run budget
   * in milliseconds; when set, the kernel terminates the run when exceeded.
   * Other axes (maxTurns, maxTokens) are set via RuntimeOptions directly.
   */
  schedulerBudget?: { maxWallMs?: number }
  tokenizer?: string
  enablePlanTool?: boolean
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
  /** Loaded via load_milestone_contract before run start. */
  milestoneContract?: MilestoneContract
  /** Custom sub-agent host driver; defaults to SubAgentOrchestrator. */
  subAgentOrchestrator?: SubAgentOrchestrator
  /**
   * When set, sub-agents run through a HarnessLoop with this config.
   * The eval provider evaluates the sub-agent's output against the criteria
   * from the AgentRunSpec, retrying up to maxAttempts times.
   */
  subAgentHarness?: {
    evalProvider: LLMProvider
    maxAttempts?: number
  }
  /** Optional system prompt injected into the dream synthesis call. */
  dreamSystemPrompt?: string
  /**
   * Optional async LLM summarizer. When provided, a background call is fired
   * after each compression event to produce a richer semantic summary.
   * The result is written back to SessionLog as `summary_upgraded` and used
   * on the next wake() in place of the rule-based summary.
   */
  asyncSummarizer?: AsyncSummarizer
}

export class RuntimeRunner {
  private interrupted = false
  private activeKernel: KernelRuntimeInstance | null = null
  private pendingObservations: KernelObservation[] = []
  private currentSessionId: string | null = null
  private nextArchiveStart = 0

  constructor(private readonly opts: RuntimeOptions) {}

  /** Host configuration (for coordinator / sub-agent spawn). */
  get hostOptions(): RuntimeOptions {
    return this.opts
  }

  /** Mount a tool capability on the currently-running kernel runtime. No-op if not running. */
  mountTool(schema: ToolSchema): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(capabilityTool(schema)))
  }

  /** Mount a skill capability on the currently-running kernel runtime. No-op if not running. */
  mountSkill(name: string, description: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilitySkill({ name, description, estimatedTokens: 0 }),
    ))
  }

  /** Mount a generic marker capability (e.g. MCP server, agent) on the active run. No-op if not running. */
  mountMarker(kind: string, id: string, description: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandMount(
      capabilityMarker(kind, id, description),
    ))
  }

  /** Unmount a capability by kind + id from the active run. No-op if not running. */
  unmountCapability(kind: string, id: string): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, capabilityCommandUnmount(kind, id))
  }

  /** Phase 4: satisfy kernel page-in requests before meta-tool execution. */
  private async applyKernelPageIn(
    runtime: KernelRuntimeInstance,
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
      if (req.tool === "memory" && this.opts.dreamStore && this.opts.agentId) {
        const hits = await this.opts.dreamStore.search(this.opts.agentId, query, topK)
        for (const hit of hits) {
          entries.push({
            content: `[memory score=${hit.score.toFixed(3)}] ${hit.text}`,
            source: "memory",
          })
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

  /** Push content into the Knowledge slot (memory retrievals, skill definitions, artifacts). */
  pushKnowledge(message: Message, tokens?: number): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, {
      kind: "add_knowledge_message",
      content: message.content ?? "",
      tokens: tokens ?? Math.max(1, Math.ceil((message.content?.length ?? 0) / 4)),
    })
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
      ...(this.opts.subAgentHarness ? { harness: this.opts.subAgentHarness } : {}),
    })

    kernelApply(runtime, this.pendingObservations, {
      kind: "sub_agent_completed",
      result: subAgentResultToKernel(result),
    })
    yield { type: "done", iterations: result.result.turnsUsed, totalTokens: result.result.totalTokensUsed, status: result.result.termination } as DoneEvent
  }

  interrupt(): void { this.interrupted = true }

  async *run(req: {
    sessionId: string
    goal: string
    criteria?: string[]
    extensions?: Record<string, unknown>
    /** Parent transcript to preload (e.g. sub-agent full context inheritance). */
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

  async *dream(agentId: string, nowMs = Date.now()): AsyncIterable<StreamEvent> {
    if (!this.opts.dreamStore) throw new Error("dreamStore not configured")
    const kernel = getKernel()

    const sessions = await this.opts.dreamStore.loadSessions(agentId)
    const existingMemories = await this.opts.dreamStore.loadMemories(agentId)
    if (!sessions.length) {
      yield { type: "done", iterations: 0, totalTokens: 0, status: "completed", dreamResult: { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 } } as DoneEvent
      return
    }

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
      yield { type: "done", iterations: 0, totalTokens: 0, status: "completed", dreamResult: { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 } } as DoneEvent
      return
    }
    if (action1.kind !== "synthesize_insights") throw new Error(`unexpected: ${action1.kind}`)

    let synthesisText = ""
    const providerState = this.opts.provider.createRunState?.()
    const synthMsgs = (action1.messages ?? []) as Message[]
    const kernelSystemText = synthMsgs.filter(m => m.role === "system").map(m => m.content).join("\n\n")
    const synthContext = {
      systemText: [kernelSystemText, this.opts.dreamSystemPrompt].filter(Boolean).join("\n\n"),
      turns: synthMsgs.filter(m => m.role !== "system"),
    }
    let totalTokens = 0
    for await (const evt of this.opts.provider.stream(synthContext, [], undefined, providerState)) {
      if (evt.type === "text_delta") { synthesisText += (evt as TextDelta).delta; yield evt }
      else if (evt.type === "usage") totalTokens = (evt as { type: string; totalTokens: number }).totalTokens
    }

    const action2 = pipeline.feedSynthesisResult(synthesisText)
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

    yield {
      type: "done", iterations: 1, totalTokens, status: "completed",
      dreamResult: {
        sessionsProcessed: rr.sessionsProcessed,
        insightsExtracted: rr.insightsExtracted,
        entriesAdded: cr.stats?.entriesAdded ?? 0,
        entriesRemoved: (cr.toRemoveIndices ?? []).length,
      },
    } as DoneEvent
  }

  /** Resolve in-kernel AskUser suspend; returns resume lists and stream events to yield. */
  private async resolveKernelSuspend(
    runtime: KernelRuntimeInstance,
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
      if (decision.approved) approved.push(g.call_id)
      else denied.push(g.call_id)
    }

    return { approved, denied, events }
  }

  private async *execute(
    sessionId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
  ): AsyncIterable<StreamEvent> {
    assertNativeProfile(this.opts)
    this.interrupted = false
    this.pendingObservations = []
    this.currentSessionId = sessionId
    const kernel = getKernel()
    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    const providerPolicy = this.opts.provider.runtimePolicy?.() ?? {}
    const effectiveMaxTurns = this.opts.maxTurns ?? providerPolicy.maxTurns ?? 25
    const effectiveTimeoutMs = this.opts.timeoutMs ?? providerPolicy.timeoutMs

    const runtime = new kernel.KernelRuntime({
      maxTokens: this.opts.maxTokens,
      maxTurns: effectiveMaxTurns,
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
    })
    this.activeKernel = runtime
    this.nextArchiveStart = nextCompressedArchiveStart
    const router = new kernel.SignalRouter(256)

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

    if (this.opts.skillDir) {
      const { scanSkillDir } = await import("../skills/loader.js")
      const metas = await scanSkillDir(this.opts.skillDir)
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_available_skills",
        skills: metas.map((m: { name: string; description: string; whenToUse?: string; effort?: number; estimatedTokens?: number }) =>
          skillMetadataToKernel({
            name: m.name,
            description: m.description,
            whenToUse: m.whenToUse,
            effort: m.effort,
            estimatedTokens: m.estimatedTokens ?? 0,
          }),
        ),
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
      kernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayed.map(messageToKernelMessage),
      })
    }

    const sessionStart = Date.now()
    const startPayload: Record<string, unknown> = {
      kind: "start_run",
      task: { goal, criteria },
    }
    if (this.opts.runSpec) {
      startPayload.run_spec = agentRunSpecToKernel(this.opts.runSpec)
    }
    // Load the declarative governance policy into the kernel before the run starts,
    // so the in-kernel gate enforces deny/veto/rate-limit/param before any tool runs.
    if (this.opts.governancePolicy) {
      kernelApply(runtime, this.pendingObservations, governancePolicyToKernelEvent(this.opts.governancePolicy))
    }
    // Enable in-kernel signal routing so the kernel owns disposition + queuing.
    if (this.opts.attentionPolicy) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_attention_policy",
        ...(this.opts.attentionPolicy.maxQueueSize !== undefined
          ? { max_queue_size: this.opts.attentionPolicy.maxQueueSize }
          : {}),
      })
    }
    // Set optional wall-clock budget override.
    if (this.opts.schedulerBudget) {
      kernelApply(runtime, this.pendingObservations, {
        kind: "set_scheduler_budget",
        ...(this.opts.schedulerBudget.maxWallMs !== undefined
          ? { max_wall_ms: this.opts.schedulerBudget.maxWallMs }
          : {}),
      })
    }
    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    let hasAttemptedReactiveCompact = false

    while (!runtime.isTerminal()) {
      // Page-in must run before appendObservations drains pending kernel observations.
      if (action.kind === "execute_tool") {
        await this.applyKernelPageIn(runtime, sessionId)
      }
      nextCompressedArchiveStart = await this.appendObservations(
        sessionId,
        runtime,
        nextCompressedArchiveStart,
      )
      this.nextArchiveStart = nextCompressedArchiveStart
      if (this.interrupted) {
        action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
        break
      }

      if (this.opts.signalSource) {
        const sig = await this.opts.signalSource.nextSignal()
        if (sig) {
          const id = crypto.randomUUID()
          const source = sig.source ?? "custom"
          const signalType = sig.signalType ?? "event"
          const urgency = sig.urgency ?? "normal"
          const summary = String((sig.payload as Record<string, unknown>)?.goal ?? sig.kind ?? "signal")
          if (this.opts.attentionPolicy) {
            // Kernel-routed: the kernel decides disposition (dedup/queue/interrupt)
            // and emits `signal_disposed`. An actionable disposition yields a new
            // action to adopt; queued/observed/ignored yields none (kernel buffers).
            // Wire shape is snake_case RuntimeSignal with an object payload.
            const sigAction = kernelMaybeAction(runtime, this.pendingObservations, {
              kind: "signal",
              signal: {
                id,
                source,
                signal_type: signalType,
                urgency,
                summary,
                payload: sig.payload ?? {},
                ...(sig.dedupeKey ? { dedupe_key: sig.dedupeKey } : {}),
                timestamp_ms: Date.now(),
              },
            })
            if (sigAction) action = sigAction
          } else if (isNativeProfile(this.opts)) {
            throw new Error(
              "COMPAT(signal-legacy): osProfile \"native\" requires attentionPolicy",
            )
          } else {
            // COMPAT(signal-legacy): SDK-side napi router (camelCase shape, string
            // payload). Used when no attentionPolicy is configured. Removable once all
            // callers drive signals through the kernel.
            const kernelSig = {
              id,
              source,
              signalType,
              urgency,
              summary,
              payload: JSON.stringify(sig.payload ?? {}),
              dedupeKey: sig.dedupeKey,
              timestampMs: Date.now(),
            }
            const disposition = router.ingest(kernelSig as unknown as Parameters<typeof router.ingest>[0], action.kind === "execute_tool")
            if (disposition === "interrupt_now") {
              action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
              break
            }
          }
        }
      }

      // COMPAT(signal-legacy): SDK-side queue drain. In kernel-routed mode the kernel
      // drains its own queue at turn boundaries, so this only runs in legacy mode.
      if (!this.opts.attentionPolicy) {
        if (isNativeProfile(this.opts)) {
          throw new Error(
            "COMPAT(signal-legacy): osProfile \"native\" forbids SDK signal queue drain",
          )
        }
        let queued = router.next()
        while (queued) {
          if (queued.urgency === "critical") {
            action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
            break
          }
          queued = router.next()
        }
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        const context = action.context
        const tools = action.tools
        let turnTokens = 0
        let turnInputTokens = 0
        let turnOutputTokens = 0
        let shouldRetry = false

        try {
          for await (const evt of this.opts.provider.stream(context, tools, Object.keys(ext).length ? ext : undefined, providerState)) {
            if (evt.type === "usage") {
              const usageEvt = evt as { type: string; totalTokens: number; inputTokens?: number; outputTokens?: number }
              turnTokens = usageEvt.totalTokens
              turnInputTokens = usageEvt.inputTokens ?? 0
              turnOutputTokens = usageEvt.outputTokens ?? 0
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
          const errMsg = String(err).toLowerCase()
          if (
            (errMsg.includes("413") || errMsg.includes("too long") || errMsg.includes("context length exceeded") || errMsg.includes("context_length_exceeded")) &&
            !hasAttemptedReactiveCompact
          ) {
            hasAttemptedReactiveCompact = true
            if (forceCompact(runtime, this.pendingObservations)) {
              nextCompressedArchiveStart = await this.appendObservations(
                sessionId,
                runtime,
                nextCompressedArchiveStart,
              )
              shouldRetry = true
            }
          }
          if (!shouldRetry) {
            yield { type: "error", message: String(err) } as ErrorEvent
            action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
            break
          }
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
          ...(this.opts.governancePolicy ? { now_ms: Date.now() } : {}),
        }
        let nextAction = kernelMaybeAction(runtime, this.pendingObservations, providerEvent)
        if (!nextAction && this.pendingObservations.some(o => o.kind === "suspended")) {
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

      } else if (action.kind === "execute_tool") {
        const allCalls: ToolCall[] = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        // In-kernel gate mode: AskUser is resolved via kernel suspend/resume before
        // execute_tool; only legacy SDK-side governance uses kernelGatedCalls here.
        const kernelGated = undefined

        const runCtx: RunContext = {
          agentId: this.opts.agentId,
          skillDir: this.opts.skillDir,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          // COMPAT(gov-sdk-gate): only pass the legacy instance when NOT in kernel-gate
          // mode, so the two gates never both run (double-gating). Remove with the field.
          governance: this.opts.governancePolicy ? undefined : this.opts.governance,
          kernelGatedCalls: kernelGated,
          onToolSuspend: this.opts.onToolSuspend,
          onPermissionRequest: this.opts.onPermissionRequest,
        }

        const toolResults: ToolResult[] = []
        const normalCalls = allCalls.filter(c => c.name !== "update_plan")
        const planCalls = allCalls.filter(c => c.name === "update_plan")

        for (const call of planCalls) {
          const update = parseUpdatePlanArgs(call.arguments)
          kernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel(update),
          })
          const result = { callId: call.id, output: "success", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: "success", isError: false } as ToolResultEvent
        }

        if (normalCalls.length > 0) {
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
          const names = normalCalls.map(c => c.name).join(", ")
          kernelApply(runtime, this.pendingObservations, {
            kind: "update_task",
            update: taskUpdateToKernel({ progress: `Executed tools: ${names}` }),
          })
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
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
        } else if (this.opts.onMilestoneEvaluate) {
          const check = await this.opts.onMilestoneEvaluate({
            phaseId: action.phaseId,
            criteria: action.criteria,
            requiredEvidence: action.requiredEvidence,
          })
          action = kernelAction(runtime, this.pendingObservations, {
            kind: "milestone_result",
            result: milestoneCheckResultToKernel(check),
          })
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
        } else {
          this.nextArchiveStart = await this.appendObservations(
            sessionId,
            runtime,
            this.nextArchiveStart,
          )
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

    const result = action.kind === "done" ? action.result : undefined
    const status = result?.termination ?? "error"
    const turnsUsed = result ? Math.max(1, result.turnsUsed) : runtime.turn() || 0
    const totalTokens = result?.totalTokensUsed ?? 0

    nextCompressedArchiveStart = await this.appendObservations(
      sessionId,
      runtime,
      nextCompressedArchiveStart,
    )
    await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
      reason: status,
      turnsUsed,
      totalTokens,
    }))

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

  private async appendObservations(
    sessionId: string,
    runtime: KernelRuntimeInstance,
    nextArchiveStart: number,
  ): Promise<number> {
    const turn = runtime.turn()
    const preservedRefs = runtime.preservedRefs()
    const observations = this.pendingObservations.splice(0)
    for (const obs of observations) {
      if (obs.kind === "page_in_requested") continue

      let archiveRef: string | undefined
      if (obs.kind === "compressed") {
        const archived = obs.archived
        if (this.opts.compressionStore && archived && archived.length > 0) {
          try {
            const pathRef = await this.opts.compressionStore.write(sessionId, nextArchiveStart, archived)
            if (pathRef) archiveRef = pathRef
          } catch {
            // non-fatal
          }
        }
      }

      const latest =
        obs.kind === "compressed" ? await this.opts.sessionLog.latestSeq(sessionId) : undefined
      const event = kernelObservationToSessionEvent(obs, turn, {
        nextArchiveStart,
        latestSeq: latest,
        archiveRef,
        preservedRefs,
        compressionAction,
      })
      if (!event) continue

      const compressedSeq = await this.opts.sessionLog.append(sessionId, event)
      if (event.kind === "compressed") {
        nextArchiveStart = compressedSeq + 1
        const archived = obs.kind === "compressed" ? obs.archived : undefined
        if (this.opts.asyncSummarizer && archived && archived.length > 0) {
          void this.upgradeCompressedSummary(
            sessionId,
            compressedSeq,
            archived as Message[],
            compressionAction(obs.action) ?? "auto_compact",
          )
        }
      }
    }
    return nextArchiveStart
  }

  private async upgradeCompressedSummary(
    sessionId: string,
    compressedSeq: number,
    archived: Message[],
    action: string,
  ): Promise<void> {
    try {
      const summary = await this.opts.asyncSummarizer!.summarize(archived, action)
      await this.opts.sessionLog.append(sessionId, {
        kind: "summary_upgraded",
        compressed_seq: compressedSeq,
        summary,
      })
    } catch {
      // non-fatal: rule-based summary stays in place
    }
  }
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

export function replayMessages(events: Array<{ seq: number; event: SessionEvent }>, maxBytes?: number): Message[] {
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
  return messages
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
      let loadedSuccessfully = false
      if (e.archive_ref && loadArchive) {
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
          loadedSuccessfully = true
        } catch (err) {
          // Loader failed (e.g. MissingArchive). We degrade and fallback.
        }
      }

      if (!loadedSuccessfully) {
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
