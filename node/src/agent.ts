import type { LLMProvider, Message, ToolCall, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import { executeTools } from "./tools/index.js"
import { readSkillFile, scanSkillDir } from "./skills/loader.js"
import type { DreamStore, DreamResult, CurationResult, MemoryEntry } from "./memory/protocols.js"
import type { KnowledgeSource } from "./knowledge/source.js"
import type { SignalSource } from "./signals/types.js"

// eslint-disable-next-line @typescript-eslint/no-explicit-any
async function loadKernel(): Promise<any> {
  const mod = await import("@deepstrike/core")
  // CJS modules imported via ESM dynamic import expose exports under `.default`
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return (mod as any).default ?? mod
}

export interface AgentOptions {
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  extensions?: Record<string, unknown>
  /**
   * System-level instructions prepended to every context render.
   * Passed to the kernel's `system` partition before the first LLM call.
   */
  systemPrompt?: string
  /**
   * Long-term memory snippets pre-seeded into the context before the first LLM call.
   * Each string is pushed to the kernel's `memory` partition (highest-priority context
   * after system). Use to inject memories retrieved from a DreamStore before a run.
   */
  initialMemory?: string[]
  /**
   * Directory containing skill `.md` files. The kernel auto-injects a `skill`
   * meta-tool so the model can load any skill by name on demand.
   */
  skillDir?: string
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  /** Backing store for the idle dreaming pipeline. Required to call `Agent.dream()`. */
  dreamStore?: DreamStore
  /**
   * Stable identifier for this agent. Required to enable in-session memory retrieval
   * when `dreamStore` is configured.
   */
  agentId?: string
  /**
   * Kernel Governance instance (from `@deepstrike/core`) or any object implementing
   * `evaluate(toolName: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }`.
   * When provided, every tool call is evaluated through the full pipeline.
   */
  governance?: { evaluate(toolName: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number } }
}

export class Agent {
  private tools = new Map<string, RegisteredTool>()
  private extensions: Record<string, unknown>
  private skillDir?: string
  private knowledgeSource?: KnowledgeSource
  private signalSource?: SignalSource
  private dreamStore?: DreamStore
  private interrupted = false
  private pendingInterrupt = false

  // Live telemetry — updated each runStreaming call
  private _turn = 0
  private _pressure = 0

  constructor(
    private readonly provider: LLMProvider,
    private readonly options: AgentOptions,
  ) {
    this.extensions = options.extensions ?? {}
    this.skillDir = options.skillDir
    this.knowledgeSource = options.knowledgeSource
    this.signalSource = options.signalSource
    this.dreamStore = options.dreamStore
  }

  /** Current turn index within the active run (0 before a run starts). */
  get turn(): number { return this._turn }

  /** Context pressure ratio [0–1] from the kernel. Values > 0.8 trigger compression. */
  get pressure(): number { return this._pressure }

  interrupt(): void {
    this.interrupted = true
  }

  register(...tools: RegisteredTool[]): this {
    for (const t of tools) this.tools.set(t.schema.name, t)
    return this
  }

  unregister(name: string): this {
    this.tools.delete(name)
    return this
  }

  /**
   * Collect the full text response and return it.
   * For richer control (streaming, tool events, token counts) use `runStreaming`.
   */
  async run(goal: string, criteria?: string[], extensions?: Record<string, unknown>): Promise<string> {
    let content = ""
    for await (const evt of this.runStreaming(goal, criteria, extensions)) {
      if (evt.type === "text_delta") content += (evt as TextDelta).delta
    }
    return content
  }

  async *runStreaming(
    goal: string,
    criteria?: string[],
    extensions?: Record<string, unknown>,
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.pendingInterrupt = false
    this._turn = 0
    this._pressure = 0

    if (this.knowledgeSource) {
      await this.knowledgeSource.init()
    }

    const kernel = await loadKernel()
    const ext = { ...this.extensions, ...(extensions ?? {}) }

    const sm = new kernel.LoopStateMachine({
      maxTokens: this.options.maxTokens,
      maxTurns: this.options.maxTurns ?? 25,
      timeoutMs: this.options.timeoutMs,
    })

    // Per-run SignalRouter — dedup state never leaks between runs.
    const router = new kernel.SignalRouter(256)

    const toolSchemas: ToolSchema[] = Array.from(this.tools.values()).map(t => t.schema)
    sm.setTools(toolSchemas)

    if (this.options.systemPrompt) {
      const tokens = Math.max(1, Math.ceil(this.options.systemPrompt.length / 4))
      sm.addSystemMessage(this.options.systemPrompt, tokens)
    }

    for (const mem of this.options.initialMemory ?? []) {
      sm.addMemoryMessage(mem, Math.max(1, Math.ceil(mem.length / 4)))
    }

    if (this.skillDir) {
      const skillMetas = await scanSkillDir(this.skillDir)
      sm.setAvailableSkills(skillMetas.map((m: { name: string; description: string; whenToUse?: string; effort?: number; estimatedTokens?: number }) => ({
        name: m.name,
        description: m.description,
        whenToUse: m.whenToUse,
        effort: m.effort,
        estimatedTokens: m.estimatedTokens ?? 0,
      })))
    }

    if (this.dreamStore && this.options.agentId) {
      sm.setMemoryEnabled(true)
    }

    if (this.knowledgeSource) {
      sm.setKnowledgeEnabled(true)
    }

    let action = sm.start({ goal, criteria: criteria ?? [] })

    const sessionStart = Date.now()
    const sessionMsgs: import("./memory/protocols.js").SessionMessage[] = [{ role: "user", content: goal }]

    while (!sm.isTerminal()) {
      // Update telemetry
      this._turn = sm.turn
      this._pressure = sm.pressure()

      // Hard interrupt
      if (this.interrupted) { action = sm.feedTimeout(); break }
      if (this.pendingInterrupt) { this.pendingInterrupt = false; action = sm.feedTimeout(); break }

      // Drain context-compression observations
      sm.takeObservations()

      // Poll signal source and route through kernel SignalRouter
      if (this.signalSource) {
        const sig = await this.signalSource.nextSignal()
        if (sig) {
          const sigAny = sig as unknown as Record<string, unknown>
          const kernelSig = {
            id: crypto.randomUUID(),
            source: (sigAny.source as string | undefined) ?? "custom",
            signalType: (sigAny.signalType as string | undefined) ?? "event",
            urgency: (sigAny.urgency as string | undefined)
              ?? (sig.kind === "interrupt" ? "critical" : "normal"),
            summary: String((sig.payload as Record<string, unknown>)?.goal ?? sig.kind),
            payload: JSON.stringify(sig.payload ?? {}),
            dedupeKey: (sigAny.dedupeKey as string | undefined) ?? null,
            timestampMs: Date.now(),
          }
          const disposition = router.ingest(kernelSig, action.kind === "execute_tools")
          if (disposition === "interrupt_now") { action = sm.feedTimeout(); break }
          if (disposition === "interrupt") this.pendingInterrupt = true
        }
      }

      // Drain previously queued signals — apply any high-urgency ones
      let queued = router.next()
      while (queued) {
        if (queued.urgency === "critical") { action = sm.feedTimeout(); break }
        if (queued.urgency === "high") this.pendingInterrupt = true
        queued = router.next()
      }
      if (this.interrupted || (sm.isTerminal())) break

      if (action.kind === "call_llm") {
        const finalToolCalls: ToolCall[] = []
        let finalText = ""
        const messages = (action.messages ?? []) as Message[]
        const tools = (action.tools ?? []) as ToolSchema[]

        let turnTokens = 0
        try {
          for await (const evt of this.provider.stream(messages, tools, Object.keys(ext).length ? ext : undefined)) {
            if (evt.type === "usage") { turnTokens = (evt as { type: string; totalTokens: number }).totalTokens; continue }
            yield evt
            if (evt.type === "text_delta") finalText += (evt as TextDelta).delta
            else if (evt.type === "tool_call") {
              const tc = evt as ToolCallEvent
              finalToolCalls.push({ id: tc.id, name: tc.name, arguments: JSON.stringify(tc.arguments) })
            }
          }
        } catch (err) {
          yield { type: "error", message: String(err) } as ErrorEvent
          action = sm.feedTimeout()
          break
        }

        action = sm.feedLlmResponse({ role: "assistant", content: finalText, toolCalls: finalToolCalls, tokenCount: turnTokens || undefined })
        sessionMsgs.push({ role: "assistant", content: finalText, toolCalls: finalToolCalls })

      } else if (action.kind === "execute_tools") {
        const allCalls: ToolCall[] = action.calls ?? []

        // Governance evaluation
        const permittedCalls: ToolCall[] = []
        const deniedResults: { callId: string; name: string; output: string; isError: boolean }[] = []
        for (const c of allCalls) {
          if (this.options.governance) {
            const verdict = this.options.governance.evaluate(c.name, c.arguments)
            if (verdict.kind === "deny") {
              const msg = `permission denied: ${c.name} — ${verdict.reason ?? ""}`
              yield { type: "error", message: msg } as ErrorEvent
              deniedResults.push({ callId: c.id, name: c.name, output: msg, isError: true })
              continue
            }
            if (verdict.kind === "rate_limited") {
              const msg = `rate limited: ${c.name} — retry after ${verdict.retryAfterMs ?? 0}ms`
              yield { type: "error", message: msg } as ErrorEvent
              deniedResults.push({ callId: c.id, name: c.name, output: msg, isError: true })
              continue
            }
            if (verdict.kind === "ask_user") {
              yield { type: "permission_request", callId: c.id, toolName: c.name, arguments: c.arguments, reason: verdict.reason ?? "" } as PermissionRequestEvent
              deniedResults.push({ callId: c.id, name: c.name, output: `awaiting user approval: ${c.name}`, isError: true })
              continue
            }
          }
          permittedCalls.push(c)
        }

        const skillCalls   = permittedCalls.filter((c: ToolCall) => c.name === "skill")
        const memoryCalls  = permittedCalls.filter((c: ToolCall) => c.name === "memory")
        const knowledgeCalls = permittedCalls.filter((c: ToolCall) => c.name === "knowledge")
        const regularCalls = permittedCalls.filter((c: ToolCall) => c.name !== "skill" && c.name !== "memory" && c.name !== "knowledge")

        const skillResults = this.skillDir
          ? await Promise.all(skillCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const name = String(args?.name ?? "")
              const content = await readSkillFile(this.skillDir!, name)
              return { callId: c.id, name: c.name, output: content ?? `Skill "${name}" not found.`, isError: !content }
            }))
          : skillCalls.map((c: ToolCall) => ({ callId: c.id, name: c.name, output: "No skill directory configured.", isError: true }))

        const memoryResults = (this.dreamStore && this.options.agentId)
          ? await Promise.all(memoryCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const query = String(args?.query ?? "")
              const topK = typeof args?.top_k === "number" ? args.top_k : 5
              const entries = await this.dreamStore!.search(this.options.agentId!, query, topK)
              const output = entries.length
                ? entries.map((e: MemoryEntry) => `[score=${e.score.toFixed(3)}] ${e.text}`).join("\n---\n")
                : "No relevant memories found."
              return { callId: c.id, name: c.name, output, isError: false }
            }))
          : memoryCalls.map((c: ToolCall) => ({ callId: c.id, name: c.name, output: "Memory retrieval not configured.", isError: true }))

        const knowledgeResults = this.knowledgeSource
          ? await Promise.all(knowledgeCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const query = String(args?.query ?? "")
              const topK = typeof args?.top_k === "number" ? args.top_k : 5
              const snippets = await this.knowledgeSource!.retrieve(query, topK)
              const output = snippets.length ? snippets.join("\n---\n") : "No relevant knowledge found."
              return { callId: c.id, name: c.name, output, isError: false }
            }))
          : knowledgeCalls.map((c: ToolCall) => ({ callId: c.id, name: c.name, output: "Knowledge source not configured.", isError: true }))

        for (const r of [...skillResults, ...memoryResults, ...knowledgeResults])
          yield { type: "tool_result", callId: r.callId, name: r.name, content: r.output, isError: r.isError } as ToolResultEvent

        const results = await executeTools(regularCalls, this.tools)
        for (const r of results) {
          const name = regularCalls.find((c: ToolCall) => c.id === r.callId)?.name ?? ""
          yield { type: "tool_result", callId: r.callId, name, content: r.output, isError: r.isError } as ToolResultEvent
        }

        action = sm.feedToolResults([
          ...deniedResults.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
          ...skillResults.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
          ...memoryResults.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
          ...knowledgeResults.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
          ...results.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
        ])

      } else if (action.kind === "done") {
        break
      }
    }

    const result = action.result
    this._turn = sm.turn
    this._pressure = sm.pressure()

    const status = result?.termination === "completed" ? "success" : (result?.termination ?? "error")
    const iterations = result ? Math.max(1, result.turnsUsed) : 0

    if (this.options.dreamStore && this.options.agentId && sessionMsgs.length > 1) {
      try {
        await this.options.dreamStore.saveSession({
          sessionId: crypto.randomUUID(),
          agentId: this.options.agentId,
          messages: sessionMsgs,
          metadata: null,
          createdAtMs: sessionStart,
          updatedAtMs: Date.now(),
        })
      } catch { /* session save failure must not surface to caller */ }
    }

    yield {
      type: "done",
      iterations,
      totalTokens: result?.totalTokensUsed ? Number(result.totalTokensUsed) : 0,
      status,
    } as DoneEvent
  }

  /**
   * Trigger the idle dreaming cycle for this agent.
   * Requires `dreamStore` and `agentId` to be configured.
   *
   * Phase 1 — kernel rule-based analysis + LLM prompt assembly
   * Phase 2 — LLM synthesis call (I/O)
   * Phase 3 — kernel parses + curates results
   * Phase 4 — commit delta to DreamStore (I/O)
   */
  async dream(agentId: string, nowMs = Date.now()): Promise<DreamResult> {
    if (!this.dreamStore) throw new Error("dreamStore not configured on AgentOptions")
    const kernel = await loadKernel()

    const sessions = await this.dreamStore.loadSessions(agentId)
    const existingMemories = await this.dreamStore.loadMemories(agentId)

    if (!sessions.length) {
      return { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 }
    }

    const kernelSessions = sessions.map(s => ({
      sessionId: s.sessionId,
      agentId: s.agentId,
      messages: s.messages.map(m => ({
        role: m.role,
        content: m.content,
        tokenCount: m.tokenCount,
        toolCalls: (m.toolCalls ?? []).map(tc => ({ id: tc.id, name: tc.name, arguments: tc.arguments })),
      })),
      metadata: JSON.stringify(s.metadata ?? null),
      createdAtMs: s.createdAtMs,
      updatedAtMs: s.updatedAtMs,
    }))
    const kernelMemories = existingMemories.map(e => ({
      text: e.text,
      score: e.score,
      metadata: JSON.stringify(e.metadata ?? null),
    }))

    const pipeline = new kernel.IdlePipeline(agentId)
    const action1 = pipeline.feedTrigger(kernelSessions, kernelMemories, nowMs)
    if (action1.kind === "noop" || action1.kind === "aborted") {
      return { sessionsProcessed: 0, insightsExtracted: 0, entriesAdded: 0, entriesRemoved: 0 }
    }
    if (action1.kind !== "synthesize_insights") {
      throw new Error(`unexpected action after feedTrigger: ${action1.kind}`)
    }

    let synthesisText = ""
    for await (const evt of this.provider.stream(
      (action1.messages ?? []) as Message[],
      [],
      undefined,
    )) {
      if (evt.type === "text_delta") synthesisText += (evt as TextDelta).delta
    }

    const action2 = pipeline.feedSynthesisResult(synthesisText)
    if (action2.kind !== "commit_memories") {
      throw new Error(`unexpected action after feedSynthesisResult: ${action2.kind}`)
    }
    const cr = action2.curationResult!
    const rr = action2.runResult!

    const dsResult: CurationResult = {
      toAdd: (cr.toAdd ?? []).map((e: MemoryEntry): MemoryEntry => ({
        text: e.text,
        score: e.score,
        metadata: tryParseJson(e.metadata as string),
      })),
      toRemoveIndices: (cr.toRemoveIndices ?? []).map(Number),
      stats: {
        insightsProcessed: cr.stats?.insightsProcessed ?? 0,
        duplicatesRemoved: cr.stats?.duplicatesRemoved ?? 0,
        conflictsResolved: cr.stats?.conflictsResolved ?? 0,
        entriesAdded: cr.stats?.entriesAdded ?? 0,
      },
    }

    await this.dreamStore.commit(agentId, dsResult, existingMemories)

    return {
      sessionsProcessed: rr.sessionsProcessed,
      insightsExtracted: rr.insightsExtracted,
      entriesAdded: cr.stats?.entriesAdded ?? 0,
      entriesRemoved: (cr.toRemoveIndices ?? []).length,
    }
  }
}

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}
