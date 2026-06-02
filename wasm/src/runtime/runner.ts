import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  ToolArgumentRepairedEvent, ToolDeniedEvent, PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse,
  DreamSummarizer,
} from "../types.js"
import type { ToolSuspendEvent } from "./execution-plane.js"
import type { DreamStore, DreamResult, MemoryEntry, CurationResult, SessionData } from "../memory/index.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import type { SignalSource } from "../signals/index.js"
import type { SessionLog, SessionEvent } from "./session-log.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { resolvePermissionRequest } from "./execution-plane.js"
import { governancePolicyToKernelEvent, type GovernancePolicy } from "../governance.js"
import { getKernel } from "./kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import { buildLlmCompletedEvent, buildRunTerminalEvent, repairEventsForRecovery } from "./session-repair.js"
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
import type { AgentRunSpec, AgentProcessChangedObservation, SubAgentResult, MilestonePolicy, MilestoneContract, MilestoneCheckResult } from "./types/agent.js"
import {
  agentRunSpecToKernel,
  findSpawnProcessObservation,
  milestoneCheckPass,
  milestoneCheckResultToKernel,
  spawnObservationToManifest,
  subAgentResultToKernel,
} from "./types/agent.js"
import { defaultSubAgentOrchestrator, type SubAgentOrchestrator } from "./sub-agent-orchestrator.js"
import { kernelObservationToSessionEvent, withCategory } from "./kernel-event-log.js"
import { DEFAULT_NATIVE_ATTENTION_POLICY, DEFAULT_NATIVE_GOVERNANCE_POLICY } from "./os-profile.js"
import { LargeResultSpool } from "./large-result-spool.js"

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
  /** Skill name → markdown body (WASM has no filesystem). */
  skillContentMap?: Map<string, string>
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  extensions?: Record<string, unknown>
  governancePolicy?: GovernancePolicy
  attentionPolicy?: { maxQueueSize?: number }
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  subAgentOrchestrator?: SubAgentOrchestrator
  milestonePolicy?: MilestonePolicy
  milestoneContract?: MilestoneContract
  onMilestoneEvaluate?: (ctx: { phaseId: string; criteria: string[]; requiredEvidence: string[] }) => Promise<MilestoneCheckResult> | MilestoneCheckResult
  runSpec?: AgentRunSpec
  dreamProvider?: LLMProvider
  dreamSummarizer?: DreamSummarizer
  dreamSystemPrompt?: string
  resultSpool?: LargeResultSpool
}

export class RuntimeRunner {
  private interrupted = false
  private pendingObservations: KernelObservation[] = []
  private activeKernel: KernelRuntimeHandle | null = null
  private currentSessionId: string | null = null
  private nextArchiveStart = 0
  private localPageOutCache: Message[] = []
  private pendingSpoolOutputs = new Map<string, { tool: string; output: string }>()

  constructor(private readonly opts: RuntimeOptions) {}

  get hostOptions(): RuntimeOptions { return this.opts }

  interrupt(): void { this.interrupted = true }

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
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
    })
    this.activeKernel = runtime

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
      kernelApply(runtime, this.pendingObservations, {
        kind: "preload_history",
        messages: replayMessages(repaired, maxBytes).map(messageToKernelMessage),
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
    const attentionPolicy = this.opts.attentionPolicy ?? DEFAULT_NATIVE_ATTENTION_POLICY
    const governancePolicy = this.opts.governancePolicy ?? DEFAULT_NATIVE_GOVERNANCE_POLICY

    kernelApply(runtime, this.pendingObservations, governancePolicyToKernelEvent(governancePolicy))
    kernelApply(runtime, this.pendingObservations, {
      kind: "set_attention_policy",
      ...(attentionPolicy.maxQueueSize !== undefined
        ? { max_queue_size: attentionPolicy.maxQueueSize }
        : {}),
    })

    let action: KernelRunnerAction = resumeMidRun
      ? kernelAction(runtime, this.pendingObservations, { kind: "resume" })
      : kernelAction(runtime, this.pendingObservations, startPayload)
    let hasAttemptedReactiveCompact = false

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
          const id = crypto.randomUUID()
          const source = sig.source ?? "custom"
          const signalType = sig.signalType ?? "event"
          const urgency = sig.urgency ?? "normal"
          const summary = String((sig.payload as Record<string, unknown>)?.goal ?? "signal")
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
              nextCompressedArchiveStart = await this.appendObservations(sessionId, runtime, nextCompressedArchiveStart)
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
        for await (const evt of this.opts.executionPlane.executeAll(allCalls, runCtx)) {
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

    const result = action.kind === "done" ? action.result : undefined
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
