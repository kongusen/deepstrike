import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  ToolSuspendEvent, ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent,
} from "../types.js"
import type { DreamStore, MemoryEntry, CurationResult, SessionData } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import type { SignalSource, RuntimeSignal } from "../signals/types.js"
import type { SessionLog, SessionEvent } from "./session-log.js"
import type { ArchiveStore } from "./archive.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
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
import { agentRunSpecToKernel, milestoneCheckPass, milestoneCheckResultToKernel, subAgentResultToKernel } from "../types/agent.js"
import { defaultSubAgentOrchestrator, type SubAgentOrchestrator } from "./sub-agent-orchestrator.js"

export interface RuntimeOptions {
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
  governance?: {
    setTime?(nowMs: bigint): void
    evaluate(name: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
  }
  tokenizer?: string
  enablePlanTool?: boolean
  compressionStore?: ArchiveStore
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
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
  /** Optional system prompt injected into the dream synthesis call. */
  dreamSystemPrompt?: string
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

  /** Push a large artifact into the kernel artifacts partition (not inlined in history). */
  pushArtifact(message: Message, tokens?: number): void {
    if (!this.activeKernel) return
    kernelApply(this.activeKernel, this.pendingObservations, {
      kind: "push_artifact",
      message: messageToKernelMessage(message),
      ...(tokens !== undefined ? { tokens } : {}),
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

    const spawned = observations.find((o): o is AgentSpawnedObservation & KernelObservation =>
      o.kind === "agent_spawned" && typeof o.agent_id === "string",
    )
    if (!spawned) throw new Error("spawn_sub_agent did not emit agent_spawned")

    const manifest: AgentSpawnedObservation = {
      kind: "agent_spawned",
      turn: spawned.turn,
      agent_id: spawned.agent_id!,
      parent_session_id: spawned.parent_session_id ?? parentSessionId,
      role: spawned.role ?? spec.role,
      isolation: spawned.isolation ?? spec.isolation ?? "shared",
      context_inheritance: spawned.context_inheritance ?? "none",
      permitted_capability_ids: spawned.permitted_capability_ids ?? [],
    }

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

  private async *execute(
    sessionId: string,
    goal: string,
    criteria: string[],
    extensions?: Record<string, unknown>,
    priorEvents?: Array<{ seq: number; event: SessionEvent }>,
    resumeMidRun = false,
  ): AsyncIterable<StreamEvent> {
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
          kind: "add_memory_message",
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
    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    let hasAttemptedReactiveCompact = false

    while (!runtime.isTerminal()) {
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
          const kernelSig = {
            id: crypto.randomUUID(),
            source: sig.source ?? "custom",
            signalType: sig.signalType ?? "event",
            urgency: sig.urgency ?? "normal",
            summary: String((sig.payload as Record<string, unknown>)?.goal ?? sig.kind ?? "signal"),
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

      let queued = router.next()
      while (queued) {
        if (queued.urgency === "critical") {
          action = kernelAction(runtime, this.pendingObservations, { kind: "timeout" })
          break
        }
        queued = router.next()
      }
      if (runtime.isTerminal()) break

      if (action.kind === "call_provider") {
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        const context = action.context
        const tools = action.tools
        let turnTokens = 0
        let shouldRetry = false

        try {
          for await (const evt of this.opts.provider.stream(context, tools, Object.keys(ext).length ? ext : undefined, providerState)) {
            if (evt.type === "usage") { turnTokens = (evt as { type: string; totalTokens: number }).totalTokens; continue }
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
          tokenCount: turnTokens || undefined,
        }
        action = kernelAction(runtime, this.pendingObservations, {
          kind: "provider_result",
          message: messageToKernelMessage(assistantMessage),
        })
        const providerReplay = peekProviderReplay(this.opts.provider, finalText, finalToolCalls)
        await this.opts.sessionLog.append(sessionId, buildLlmCompletedEvent({
          turn: runtime.turn(),
          content: finalText,
          tokenCount: turnTokens || undefined,
          toolCalls: finalToolCalls,
          providerReplay,
        }))

      } else if (action.kind === "execute_tool") {
        const allCalls: ToolCall[] = action.calls
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: runtime.turn(), calls: allCalls })

        const runCtx: RunContext = {
          agentId: this.opts.agentId,
          skillDir: this.opts.skillDir,
          dreamStore: this.opts.dreamStore,
          knowledgeSource: this.opts.knowledgeSource,
          governance: this.opts.governance,
          onToolSuspend: this.opts.onToolSuspend,
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
                arguments: JSON.stringify(pre.arguments),
                reason: pre.reason,
              })
              await this.opts.sessionLog.append(sessionId, {
                kind: "permission_resolved",
                turn,
                approved: false,
                responder: "policy_gate",
              })
              await this.opts.sessionLog.append(sessionId, {
                kind: "tool_denied",
                turn,
                call_id: pre.callId,
                tool_name: pre.toolName,
                reason: `permission denied by policy gate: ${pre.reason}`,
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
      if (obs.kind === "compressed") {
        const latest = await this.opts.sessionLog.latestSeq(sessionId)
        if (latest < nextArchiveStart) continue
        const end = latest

        let archiveRef: string | undefined = undefined
        const archived = obs.archived
        if (this.opts.compressionStore && archived && archived.length > 0) {
          try {
            const pathRef = await this.opts.compressionStore.write(sessionId, nextArchiveStart, archived)
            if (pathRef) archiveRef = pathRef
          } catch {
            // non-fatal
          }
        }

        const compressedSeq = await this.opts.sessionLog.append(sessionId, {
          kind: "compressed",
          turn,
          archived_seq_range: [nextArchiveStart, end],
          action: compressionAction(obs.action),
          summary: obs.summary,
          summary_tokens: obs.summary ? Math.max(1, Math.ceil(obs.summary.length / 4)) : undefined,
          archive_ref: archiveRef,
          preserved_refs: preservedRefs,
        })
        nextArchiveStart = compressedSeq + 1
      } else if (obs.kind === "rollbacked") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "rollbacked",
          turn: obs.turn ?? turn,
          checkpoint_history_len: obs.checkpoint_history_len ?? 0,
          reason: obs.reason,
        })
      } else if (obs.kind === "capability_changed") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "capability_changed",
          turn: obs.turn ?? turn,
          added: obs.added ?? [],
          removed: obs.removed ?? [],
          ...(obs.change_kind != null && { change_kind: obs.change_kind }),
          ...(obs.capability_id != null && { capability_id: obs.capability_id }),
          ...(obs.version != null && { version: obs.version }),
          ...(obs.mounted_by != null && { mounted_by: obs.mounted_by }),
          ...(obs.mount_reason != null && { mount_reason: obs.mount_reason }),
        })
      } else if (obs.kind === "milestone_advanced") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "milestone_advanced",
          turn: obs.turn ?? turn,
          phase_id: obs.phase_id ?? "",
          capabilities_unlocked: obs.capabilities_unlocked ?? [],
        })
      } else if (obs.kind === "milestone_blocked") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "milestone_blocked",
          turn: obs.turn ?? turn,
          phase_id: obs.phase_id ?? "",
          reason: typeof obs.reason === "string" ? obs.reason : "",
        })
      } else if (obs.kind === "milestone_evidence") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "milestone_evidence",
          turn: obs.turn ?? turn,
          phase_id: obs.phase_id ?? "",
          evidence: obs.evidence ?? [],
        })
      } else if (obs.kind === "checkpoint_taken") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "checkpoint_taken",
          turn: obs.turn ?? turn,
          history_len: obs.history_len ?? 0,
        })
      } else if (obs.kind === "agent_spawned") {
        await this.opts.sessionLog.append(sessionId, {
          kind: "agent_spawned",
          turn: obs.turn ?? turn,
          agent_id: obs.agent_id ?? "",
          parent_session_id: obs.parent_session_id ?? "",
          role: obs.role ?? "",
          isolation: obs.isolation ?? "",
          context_inheritance: obs.context_inheritance ?? "",
          permitted_capability_ids: obs.permitted_capability_ids ?? [],
        })
      } else if (obs.kind !== "renewed") {
        console.warn(`[deepstrike] unhandled KernelObservation kind: ${obs.kind}`)
      }
    }
    return nextArchiveStart
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
  const messages: Message[] = []
  for (const { event: e } of events) {
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
      if (e.summary) {
        const systemText = `[Compressed context: turn ${e.turn}]\n${e.summary}`
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
  const messages: Message[] = []
  for (const { event: e } of events) {
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
