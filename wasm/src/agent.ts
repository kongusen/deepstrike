import type { LLMProvider, Message, ToolCall, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent, ErrorEvent, PermissionRequestEvent } from "./types.js"
import type { RegisteredTool } from "./tools/index.js"
import { executeTools } from "./tools/index.js"
import type { KnowledgeSource } from "./knowledge/index.js"
import type { SignalSource } from "./signals/index.js"
import type { DreamStore } from "./memory/index.js"
import { Governance } from "./governance.js"

export interface SkillMetadata {
  name: string
  description: string
  whenToUse?: string
  allowedTools?: string[]
  effort?: number
  estimatedTokens?: number
}

type WasmKernel = typeof import("@deepstrike/wasm-kernel")
async function loadKernel(): Promise<WasmKernel> {
  return import("@deepstrike/wasm-kernel") as Promise<WasmKernel>
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
   * Each string is pushed to the kernel's `memory` partition.
   */
  initialMemory?: string[]
  skillDir?: string
  knowledgeSource?: KnowledgeSource
  signalSource?: SignalSource
  dreamStore?: DreamStore
  agentId?: string
  governance?: Governance
  /** Host-provided skill content map (name → markdown body). WASM has no fs access. */
  skillContentMap?: Map<string, string>
}

export class Agent {
  private tools = new Map<string, RegisteredTool>()
  private blockedTools = new Set<string>()
  private extensions: Record<string, unknown>
  private interrupted = false
  private pendingInterrupt = false
  private _pendingSkills: SkillMetadata[] = []

  constructor(
    private readonly provider: LLMProvider,
    private readonly options: AgentOptions,
  ) {
    this.extensions = options.extensions ?? {}
  }

  register(...tools: RegisteredTool[]): this {
    for (const t of tools) this.tools.set(t.schema.name, t)
    return this
  }

  unregister(name: string): this { this.tools.delete(name); return this }
  blockTool(name: string): this { this.blockedTools.add(name); return this }
  interrupt(): void { this.interrupted = true }

  async run(goal: string, criteria?: string[], extensions?: Record<string, unknown>): Promise<string> {
    let text = ""
    for await (const evt of this.runStreaming(goal, criteria, extensions)) {
      if (evt.type === "text_delta") text += (evt as TextDelta).delta
    }
    return text
  }

  async *runStreaming(
    goal: string,
    criteria?: string[],
    extensions?: Record<string, unknown>,
  ): AsyncIterable<StreamEvent> {
    this.interrupted = false
    this.pendingInterrupt = false

    if (this.options.knowledgeSource) {
      await this.options.knowledgeSource.init()
    }

    const kernel = await loadKernel()
    if (this.options.governance) this.options.governance._attach(kernel)
    const ext = { ...this.extensions, ...(extensions ?? {}) }

    const sm = new kernel.LoopStateMachine({
      maxTokens: this.options.maxTokens,
      maxTurns: this.options.maxTurns ?? 25,
      timeoutMs: this.options.timeoutMs,
    })
    sm.setTools(Array.from(this.tools.values()).map(t => t.schema))

    if (this.options.systemPrompt) {
      const tokens = Math.max(1, Math.ceil(this.options.systemPrompt.length / 4))
      sm.addSystemMessage(this.options.systemPrompt, tokens)
    }

    for (const mem of this.options.initialMemory ?? []) {
      sm.addMemoryMessage(mem, Math.max(1, Math.ceil(mem.length / 4)))
    }

    if (this._pendingSkills.length > 0) {
      sm.setAvailableSkills(this._pendingSkills)
    }

    if (this.options.dreamStore && this.options.agentId) sm.setMemoryEnabled(true)
    if (this.options.knowledgeSource) sm.setKnowledgeEnabled(true)

    const router = new kernel.SignalRouter(256)

    let action = sm.start({ goal, criteria: criteria ?? [] })
    let finalText = ""

    const sessionStart = Date.now()
    const sessionMsgs: import("./memory/index.js").SessionMessage[] = [{ role: "user", content: goal }]

    while (!sm.isTerminal()) {
      if (this.interrupted) { action = sm.feedTimeout(); break }
      if (this.pendingInterrupt) { this.pendingInterrupt = false; action = sm.feedTimeout(); break }

      if (this.options.signalSource) {
        const sig = await this.options.signalSource.nextSignal()
        if (sig) {
          const kernelSig = {
            id: crypto.randomUUID(),
            source: sig.source,
            signalType: sig.signalType,
            urgency: sig.urgency,
            summary: String(sig.payload?.goal ?? sig.signalType),
            payload: JSON.stringify(sig.payload ?? {}),
            dedupeKey: sig.dedupeKey,
            timestampMs: Date.now(),
          }
          const disposition = router.ingest(kernelSig, action.kind === "execute_tools")
          if (disposition === "interrupt_now") { action = sm.feedTimeout(); break }
          if (disposition === "interrupt") this.pendingInterrupt = true
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
        sessionMsgs.push({ role: "assistant", content: finalText, toolCalls: finalToolCalls })

      } else if (action.kind === "execute_tools") {
        const allCalls = (action.calls ?? []) as ToolCall[]

        // Governance check
        const permittedCalls: ToolCall[] = []
        for (const c of allCalls) {
          if (this.blockedTools.has(c.name)) {
            yield { type: "error", message: `tool blocked: ${c.name}` } as ErrorEvent
            continue
          }
          if (this.options.governance) {
            this.options.governance.setTime(Date.now())
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

        const skillCalls = permittedCalls.filter(c => c.name === "skill")
        const memoryCalls = permittedCalls.filter(c => c.name === "memory")
        const knowledgeCalls = permittedCalls.filter(c => c.name === "knowledge")
        const regularCalls = permittedCalls.filter(c => !["skill", "memory", "knowledge"].includes(c.name))

        // skill: WASM host must provide skill content via a registered tool or extension
        const skillContentMap = this.options.skillContentMap ?? new Map<string, string>()
        const skillResults = skillCalls.map(c => {
          const args = tryParseJson(c.arguments) as Record<string, unknown>
          const name = String(args?.name ?? "")
          const content = skillContentMap.get(name)
          const output = content ?? `Skill "${name}" not found.`
          return { callId: c.id, output, isError: content === undefined }
        })

        const memoryResults: Array<{ callId: string; output: string; isError: boolean }> = []
        if (this.options.dreamStore && this.options.agentId) {
          for (const c of memoryCalls) {
            const args = tryParseJson(c.arguments) as Record<string, unknown>
            const query = String(args?.query ?? "")
            const topK = typeof args?.top_k === "number" ? args.top_k : 5
            const entries = await this.options.dreamStore.search(this.options.agentId, query, topK)
            const output = entries.length ? entries.map(e => `[score=${e.score.toFixed(3)}] ${e.text}`).join("\n---\n") : "No relevant memories found."
            yield { type: "tool_result", callId: c.id, name: c.name, content: output, isError: false } as ToolResultEvent
            memoryResults.push({ callId: c.id, output, isError: false })
          }
        } else {
          for (const c of memoryCalls) memoryResults.push({ callId: c.id, output: "Memory retrieval not configured.", isError: true })
        }

        const knowledgeResults: Array<{ callId: string; output: string; isError: boolean }> = []
        if (this.options.knowledgeSource) {
          for (const c of knowledgeCalls) {
            const args = tryParseJson(c.arguments) as Record<string, unknown>
            const query = String(args?.query ?? "")
            const topK = typeof args?.top_k === "number" ? args.top_k : 5
            const snippets = await this.options.knowledgeSource.retrieve(query, topK)
            const output = snippets.length ? snippets.join("\n---\n") : "No relevant knowledge found."
            yield { type: "tool_result", callId: c.id, name: c.name, content: output, isError: false } as ToolResultEvent
            knowledgeResults.push({ callId: c.id, output, isError: false })
          }
        } else {
          for (const c of knowledgeCalls) knowledgeResults.push({ callId: c.id, output: "Knowledge source not configured.", isError: true })
        }

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
      iterations: result?.turnsUsed ?? 0,
      totalTokens: Number(result?.totalTokensUsed ?? 0),
      status: result?.termination ?? "error",
    } as DoneEvent
  }

  /** Register available skills from host-provided metadata (WASM has no fs access). */
  setAvailableSkills(skills: SkillMetadata[]): void {
    this._pendingSkills = skills
  }
}

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}
