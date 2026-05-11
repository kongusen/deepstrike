import type { LLMProvider, Message, ToolCall, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import { executeTools } from "./tools/index.js"
import { readSkillFile, scanSkillDir } from "./skills/loader.js"
import type { DreamStore, DreamResult, CurationResult, MemoryEntry } from "./memory/protocols.js"
import type { KnowledgeSource } from "./knowledge/source.js"
import type { SignalSource } from "./signals/types.js"

type KernelModule = typeof import("@deepstrike/core")

async function loadKernel(): Promise<KernelModule> {
  return import("@deepstrike/core")
}

export interface AgentOptions {
  maxTokens: number
  maxTurns?: number
  timeoutMs?: number
  extensions?: Record<string, unknown>
  /**
   * Directory containing skill `.md` files. The kernel will auto-inject a
   * `skill` meta-tool so the model can load any skill by name on demand.
   */
  skillDir?: string
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  /** Backing store for the idle dreaming pipeline. Required to call `Agent.dream()`. */
  dreamStore?: DreamStore
  /**
   * Stable identifier for this agent. Required to enable in-session memory retrieval
   * when `dreamStore` is configured — the kernel injects a `memory` meta-tool and
   * the SDK calls `dreamStore.search(agentId, query)` on demand.
   */
  agentId?: string
  /** Governance instance from @deepstrike/core for permission checks on tool calls. */
  governance?: import("@deepstrike/core").Governance
}

export class Agent {
  private tools = new Map<string, RegisteredTool>()
  private blockedTools = new Set<string>()
  private extensions: Record<string, unknown>
  private skillDir?: string
  private knowledgeSource?: KnowledgeSource
  private signalSource?: SignalSource
  private dreamStore?: DreamStore
  private interrupted = false
  private pendingInterrupt = false

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

  blockTool(name: string): this {
    this.blockedTools.add(name)
    return this
  }

  async run(goal: string, criteria?: string[], extensions?: Record<string, unknown>): Promise<string> {
    let result: DoneEvent | undefined
    for await (const evt of this.runStreaming(goal, criteria, extensions)) {
      if (evt.type === "done") result = evt as DoneEvent
    }
    return result ? `done in ${result.iterations} turns (${result.status})` : "done"
  }

  async *runStreaming(
    goal: string,
    criteria?: string[],
    extensions?: Record<string, unknown>,
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.pendingInterrupt = false
    const kernel = await loadKernel()
    const ext = { ...this.extensions, ...(extensions ?? {}) }

    const sm = new kernel.LoopStateMachine({
      maxTokens: this.options.maxTokens,
      maxTurns: this.options.maxTurns ?? 25,
      timeoutMs: this.options.timeoutMs,
    })

    // Create a per-run SignalRouter so dedup state doesn't leak across runs.
    const router = new kernel.SignalRouter(256)

    const toolSchemas: ToolSchema[] = Array.from(this.tools.values()).map(t => t.schema)
    sm.setTools(toolSchemas)

    // Scan skill directory and register metadata with the kernel.
    // The kernel will auto-inject the `skill` meta-tool into every CallLLM action.
    if (this.skillDir) {
      const skillMetas = await scanSkillDir(this.skillDir)
      sm.setAvailableSkills(skillMetas.map(m => ({
        name: m.name,
        description: m.description,
        whenToUse: m.whenToUse,
        effort: m.effort,
        estimatedTokens: m.estimatedTokens ?? 0,
      })))
    }

    // Enable the memory meta-tool when both dreamStore and agentId are provided.
    if (this.dreamStore && this.options.agentId) {
      sm.setMemoryEnabled(true)
    }

    // Enable the knowledge meta-tool when a KnowledgeSource is configured.
    if (this.knowledgeSource) {
      sm.setKnowledgeEnabled(true)
    }

    let action = sm.start({ goal, criteria: criteria ?? [] })
    let finalText = ""

    while (!sm.isTerminal()) {
      if (this.interrupted) { action = sm.feedTimeout(); break }
      if (this.pendingInterrupt) { this.pendingInterrupt = false; action = sm.feedTimeout(); break }

      // Poll signal source and route through kernel SignalRouter
      if (this.signalSource) {
        const sig = await this.signalSource.nextSignal()
        if (sig) {
          const kernelSig = {
            id: crypto.randomUUID(),
            source: (sig as any).source ?? "custom",
            signalType: (sig as any).signalType ?? "event",
            urgency: (sig as any).urgency ?? (sig.kind === "interrupt" ? "critical" : "normal"),
            summary: String((sig.payload as any)?.goal ?? sig.kind),
            payload: JSON.stringify(sig.payload ?? {}),
            dedupeKey: (sig as any).dedupeKey ?? null,
            timestampMs: Date.now(),
          }
          const disposition = router.ingest(kernelSig, action.kind === "execute_tools")
          if (disposition === "interrupt_now") { action = sm.feedTimeout(); break }
          if (disposition === "interrupt") { this.pendingInterrupt = true }
          // "queue" → buffered; "observe" / "ignore" / "dropped" → no action
        }
      }

      sm.takeObservations()

      if (action.kind === "call_llm") {
        finalText = ""
        const finalToolCalls: ToolCall[] = []
        const messages = (action.messages ?? []) as Message[]
        const tools = (action.tools ?? []) as ToolSchema[]

        try {
          for await (const evt of this.provider.stream(messages, tools, Object.keys(ext).length ? ext : undefined)) {
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

        action = sm.feedLlmResponse({ role: "assistant", content: finalText, toolCalls: finalToolCalls })

      } else if (action.kind === "execute_tools") {
        const allCalls: ToolCall[] = action.calls ?? []

        // Governance check: blocked tools + GovernancePipeline
        const permittedCalls: ToolCall[] = []
        for (const c of allCalls) {
          if (this.blockedTools.has(c.name)) {
            yield { type: "error", message: `tool blocked: ${c.name}` } as ErrorEvent
            continue
          }
          if (this.options.governance) {
            const verdict = this.options.governance.evaluate(c.name, c.arguments)
            if (verdict.kind === "deny") {
              yield { type: "error", message: `permission denied: ${c.name} — ${verdict.reason}` } as ErrorEvent
              continue
            }
            if (verdict.kind === "ask_user") {
              yield { type: "permission_request", callId: c.id, toolName: c.name, arguments: c.arguments, reason: verdict.reason } as PermissionRequestEvent
              continue
            }
          }
          permittedCalls.push(c)
        }
        const calls = permittedCalls

        // Intercept `skill` meta-tool calls: read file, return content as tool result.
        const skillCalls = calls.filter((c: ToolCall) => c.name === "skill")
        // Intercept `memory` meta-tool calls: search DreamStore, return entries as tool result.
        const memoryCalls = calls.filter((c: ToolCall) => c.name === "memory")
        // Intercept `knowledge` meta-tool calls: retrieve from KnowledgeSource.
        const knowledgeCalls = calls.filter((c: ToolCall) => c.name === "knowledge")
        const regularCalls = calls.filter((c: ToolCall) => c.name !== "skill" && c.name !== "memory" && c.name !== "knowledge")

        const skillResults = this.skillDir
          ? await Promise.all(skillCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const name = String(args?.name ?? "")
              const content = await readSkillFile(this.skillDir!, name)
              const output = content ?? `Skill "${name}" not found.`
              yield { type: "tool_result", callId: c.id, name: c.name, content: output, isError: !content } as ToolResultEvent
              return { callId: c.id, output, isError: !content }
            }))
          : skillCalls.map((c: ToolCall) => {
              const output = "No skill directory configured."
              return { callId: c.id, output, isError: true }
            })

        const memoryResults = (this.dreamStore && this.options.agentId)
          ? await Promise.all(memoryCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const query = String(args?.query ?? "")
              const topK = typeof args?.top_k === "number" ? args.top_k : 5
              const entries = await this.dreamStore!.search(this.options.agentId!, query, topK)
              const output = entries.length
                ? entries.map(e => `[score=${e.score.toFixed(3)}] ${e.text}`).join("\n---\n")
                : "No relevant memories found."
              yield { type: "tool_result", callId: c.id, name: c.name, content: output, isError: false } as ToolResultEvent
              return { callId: c.id, output, isError: false }
            }))
          : memoryCalls.map((c: ToolCall) => {
              const output = "Memory retrieval not configured."
              return { callId: c.id, output, isError: true }
            })

        const knowledgeResults = this.knowledgeSource
          ? await Promise.all(knowledgeCalls.map(async (c: ToolCall) => {
              const args = tryParseJson(c.arguments) as Record<string, unknown>
              const query = String(args?.query ?? "")
              const topK = typeof args?.top_k === "number" ? args.top_k : 5
              const snippets = await this.knowledgeSource!.retrieve(query, topK)
              const output = snippets.length ? snippets.join("\n---\n") : "No relevant knowledge found."
              yield { type: "tool_result", callId: c.id, name: c.name, content: output, isError: false } as ToolResultEvent
              return { callId: c.id, output, isError: false }
            }))
          : knowledgeCalls.map((c: ToolCall) => ({ callId: c.id, output: "Knowledge source not configured.", isError: true }))

        const results = await executeTools(regularCalls, this.tools)
        for (const r of results) {
          const name = regularCalls.find(c => c.id === r.callId)?.name ?? ""
          yield { type: "tool_result", callId: r.callId, name, content: r.output, isError: r.isError } as ToolResultEvent
        }

        action = sm.feedToolResults([
          ...skillResults,
          ...memoryResults,
          ...knowledgeResults,
          ...results.map(r => ({ callId: r.callId, output: r.output, isError: r.isError })),
        ])

      } else if (action.kind === "done") {
        break
      }
    }

    const result = action.result
    yield {
      type: "done",
      iterations: result?.turnsUsed ?? 0,
      totalTokens: Number(result?.totalTokensUsed ?? 0),
      status: result?.termination ?? "error",
    } as DoneEvent

  }

  /**
   * Trigger an idle dreaming cycle for the given agent.
   *
   * Phase 1 — kernel rule-based analysis + LLM prompt assembly (kernel, pure computation)
   * Phase 2 — LLM synthesis call (SDK, I/O here)
   * Phase 3 — kernel parses + curates results (kernel, pure computation)
   * Phase 4 — commit delta to DreamStore (SDK, I/O here)
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
        toolCalls: (m.toolCalls ?? []).map(tc => ({
          id: tc.id, name: tc.name, arguments: tc.arguments,
        })),
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
    if (action1.kind === "noop") {
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
      toAdd: (cr.toAdd ?? []).map((e): MemoryEntry => ({
        text: e.text,
        score: e.score,
        metadata: tryParseJson(e.metadata),
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
