import type {
  LLMProvider, Message, ToolCall, ToolResult, ToolSchema,
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent,
  ToolSuspendEvent,
} from "../types.js"
import type { DreamStore, DreamResult, MemoryEntry, CurationResult, SessionData } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import type { SignalSource, RuntimeSignal } from "../signals/types.js"
import type { SessionLog, SessionEvent } from "./session-log.js"
import type { ArchiveStore } from "./archive.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { getKernel } from "../kernel.js"
import { peekProviderReplay, seedProviderReplayFromEvents } from "./provider-replay.js"
import { sanitizeReplayText } from "./replay-sanitize.js"
import { buildLlmCompletedEvent, buildRunTerminalEvent, repairEventsForRecovery } from "./session-repair.js"

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
}

export class RuntimeRunner {
  private interrupted = false

  constructor(private readonly opts: RuntimeOptions) {}

  interrupt(): void { this.interrupted = true }

  async *run(req: {
    sessionId: string
    goal: string
    criteria?: string[]
    extensions?: Record<string, unknown>
  }): AsyncIterable<StreamEvent> {
    const prior = await this.opts.sessionLog.read(req.sessionId)
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

  async dream(agentId: string, nowMs = Date.now()): Promise<DreamResult> {
    if (!this.opts.dreamStore) throw new Error("dreamStore not configured")
    const kernel = getKernel()

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
    const providerState = this.opts.provider.createRunState?.()
    const synthMsgs = (action1.messages ?? []) as Message[]
    const synthContext = {
      systemText: synthMsgs.filter(m => m.role === "system").map(m => m.content).join("\n\n"),
      turns: synthMsgs.filter(m => m.role !== "system"),
    }
    for await (const evt of this.opts.provider.stream(synthContext, [], undefined, providerState)) {
      if (evt.type === "text_delta") synthesisText += (evt as TextDelta).delta
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
    const kernel = getKernel()
    const ext = { ...this.opts.extensions, ...(extensions ?? {}) }
    const providerState = this.opts.provider.createRunState?.()
    let nextCompressedArchiveStart = nextArchivedSeqStart(priorEvents)

    // Three-layer policy merge: explicit RuntimeOptions > provider.runtimePolicy() > defaults
    const providerPolicy = this.opts.provider.runtimePolicy?.() ?? {}
    const effectiveMaxTurns   = this.opts.maxTurns   ?? providerPolicy.maxTurns   ?? 25
    const effectiveTimeoutMs  = this.opts.timeoutMs  ?? providerPolicy.timeoutMs

    const sm = new kernel.LoopStateMachine({
      maxTokens: this.opts.maxTokens,
      maxTurns: effectiveMaxTurns,
      timeoutMs: effectiveTimeoutMs !== undefined ? BigInt(effectiveTimeoutMs) : undefined,
    })
    const router = new kernel.SignalRouter(256)

    if (this.opts.tokenizer) {
      sm.setTokenizer(this.opts.tokenizer)
    }
    if (this.opts.enablePlanTool !== undefined) {
      sm.setPlanToolEnabled(this.opts.enablePlanTool)
    }

    sm.setTools(this.opts.executionPlane.schemas())

    if (this.opts.systemPrompt) {
      sm.addSystemMessage(this.opts.systemPrompt, Math.max(1, Math.ceil(this.opts.systemPrompt.length / 4)))
    }

    if (this.opts.initialMemory) {
      for (const mem of this.opts.initialMemory) {
        sm.addMemoryMessage(mem, Math.max(1, Math.ceil(mem.length / 4)))
      }
    }

    if (this.opts.skillDir) {
      const { scanSkillDir } = await import("../skills/loader.js")
      const metas = await scanSkillDir(this.opts.skillDir)
      sm.setAvailableSkills(metas.map((m: { name: string; description: string; whenToUse?: string; effort?: number; estimatedTokens?: number }) => ({
        name: m.name, description: m.description, whenToUse: m.whenToUse,
        effort: m.effort, estimatedTokens: m.estimatedTokens ?? 0,
      })))
    }

    if (this.opts.dreamStore && this.opts.agentId) sm.setMemoryEnabled(true)
    if (this.opts.knowledgeSource) sm.setKnowledgeEnabled(true)

    const maxBytes = sm.recoveryContentBytes()

    if (priorEvents && priorEvents.length > 0) {
      const repaired = repairEventsForRecovery(priorEvents, maxBytes)
      seedProviderReplayFromEvents(this.opts.provider, repaired)
      sm.preloadHistory(replayMessages(repaired, maxBytes))
    }

    const sessionStart = Date.now()
    let action = resumeMidRun
      ? sm.resumeAfterPreload()
      : sm.start({ goal, criteria })
    let hasAttemptedReactiveCompact = false

    while (!sm.isTerminal()) {
      nextCompressedArchiveStart = await this.appendObservations(sessionId, sm, nextCompressedArchiveStart)
      if (this.interrupted) { action = sm.feedTimeout(); break }

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
          const disposition = router.ingest(kernelSig as unknown as Parameters<typeof router.ingest>[0], action.kind === "execute_tools")
          if (disposition === "interrupt_now") { action = sm.feedTimeout(); break }
        }
      }

      let queued = router.next()
      while (queued) {
        if (queued.urgency === "critical") { action = sm.feedTimeout(); break }
        queued = router.next()
      }
      if (sm.isTerminal()) break

      if (action.kind === "call_llm") {
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        let context = (action as unknown as { context: import("../types.js").RenderedContext }).context
        const tools = (action.tools ?? []) as ToolSchema[]
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
            const compacted = forceCompact(sm)
            if (compacted) {
              nextCompressedArchiveStart = await this.appendObservations(sessionId, sm, nextCompressedArchiveStart)
              shouldRetry = true
            }
          }
          if (!shouldRetry) {
            yield { type: "error", message: String(err) } as ErrorEvent
            action = sm.feedTimeout()
            break
          }
        }

        if (shouldRetry) {
          (action as any).context = sm.render()
          continue
        }

        action = sm.feedLlmResponse({
          role: "assistant",
          content: finalText,
          toolCalls: finalToolCalls,
          tokenCount: turnTokens || undefined,
        })
        const providerReplay = peekProviderReplay(this.opts.provider, finalText, finalToolCalls)
        await this.opts.sessionLog.append(sessionId, buildLlmCompletedEvent({
          turn: sm.turn,
          content: finalText,
          tokenCount: turnTokens || undefined,
          toolCalls: finalToolCalls,
          providerReplay,
        }))

      } else if (action.kind === "execute_tools") {
        const allCalls: ToolCall[] = action.calls ?? []
        await this.opts.sessionLog.append(sessionId, { kind: "tool_requested", turn: sm.turn, calls: allCalls })

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
          sm.updateTask(update)
          const result = { callId: call.id, output: "success", isError: false }
          toolResults.push(result)
          yield { type: "tool_result", callId: call.id, content: "success", isError: false } as ToolResultEvent
        }

        if (normalCalls.length > 0) {
          for await (const evt of this.opts.executionPlane.executeAll(normalCalls, runCtx)) {
            yield evt
            if (evt.type === "tool_result") {
              const tre = evt as ToolResultEvent
              toolResults.push({ callId: tre.callId, output: tre.content, isError: tre.isError })
            }
          }
          const names = normalCalls.map(c => c.name).join(", ")
          sm.updateTask({ progress: `Executed tools: ${names}` })
        }

        await this.opts.sessionLog.append(sessionId, {
          kind: "tool_completed",
          turn: sm.turn,
          results: toolResults.map(r => ({
            call_id: r.callId,
            output: r.output,
            is_error: r.isError,
            token_count: r.tokenCount,
          })),
        })
        action = sm.feedToolResults(toolResults)

      } else if (action.kind === "done") {
        break
      }
    }

    const result = action.result
    const status = result?.termination ?? "error"
    const turnsUsed = result ? Math.max(1, result.turnsUsed) : 0
    const totalTokens = result?.totalTokensUsed ? Number(result.totalTokensUsed) : 0

    nextCompressedArchiveStart = await this.appendObservations(sessionId, sm, nextCompressedArchiveStart)
    await this.opts.sessionLog.append(sessionId, buildRunTerminalEvent({
      reason: status,
      turnsUsed,
      totalTokens,
    }))

    if (this.opts.dreamStore && this.opts.agentId) {
      const newMsgs = (sm.drainNewMessages() as Message[]).map(m => ({
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
  }

  private async appendObservations(
    sessionId: string,
    sm: { turn: number; takeObservations(): Array<any> },
    nextArchiveStart: number,
  ): Promise<number> {
    const observations = sm.takeObservations()
    for (const obs of observations) {
      if (obs.kind !== "compressed") continue
      const latest = await this.opts.sessionLog.latestSeq(sessionId)
      if (latest < nextArchiveStart) continue
      const end = latest

      let archiveRef: string | undefined = undefined
      const archived = obs.archived
      if (this.opts.compressionStore && archived && archived.length > 0) {
        try {
          const pathRef = await this.opts.compressionStore.write(sessionId, nextArchiveStart, archived)
          if (pathRef) archiveRef = pathRef
        } catch (err) {
          // ignore or log
        }
      }

      const compressedSeq = await this.opts.sessionLog.append(sessionId, {
        kind: "compressed",
        turn: sm.turn,
        archived_seq_range: [nextArchiveStart, end],
        action: obs.action,
        summary: obs.summary,
        summary_tokens: obs.summary ? Math.max(1, Math.ceil(obs.summary.length / 4)) : undefined,
        archive_ref: archiveRef,
        preserved_refs: (sm as any).ctx?.partitions?.task_state?.preserved_refs ?? [],
      })
      nextArchiveStart = compressedSeq + 1
    }
    return nextArchiveStart
  }
}

function isMidRun(events: Array<{ seq: number; event: SessionEvent }>): boolean {
  return events.length > 0 && !events.some(e => e.event.kind === "run_terminal")
}

function forceCompact(sm: { forceCompact?: () => boolean; force_compact?: () => boolean }): boolean {
  if (typeof sm.forceCompact === "function") return sm.forceCompact()
  if (typeof sm.force_compact === "function") return sm.force_compact()
  throw new TypeError("LoopStateMachine forceCompact binding is unavailable")
}

function replayMessages(events: Array<{ seq: number; event: SessionEvent }>, maxBytes?: number): Message[] {
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

function parseUpdatePlanArgs(argsStr: string): any {
  let parsed: any = {}
  try {
    parsed = JSON.parse(argsStr)
  } catch {
    // Ignore parse error
  }
  return {
    plan: parsed.plan,
    currentStep: parsed.currentStep !== undefined ? parsed.currentStep : parsed.current_step,
    progress: parsed.progress,
    scratchpad: parsed.scratchpad,
    blockedOn: parsed.blockedOn !== undefined ? parsed.blockedOn : parsed.blocked_on,
  }
}
