import type {
  ToolCall, ToolSchema, StreamEvent, ToolResultEvent, PermissionRequestEvent, ToolDeniedEvent,
} from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { DreamStore, MemoryEntry } from "../memory/index.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import type { Governance } from "../governance.js"

export interface ToolSuspendEvent {
  type: "tool_suspend"
  callId: string
  name: string
  suspensionId: string
  payload?: unknown
}

export interface RunContext {
  agentId?: string
  skillContentMap?: Map<string, string>
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  governance?: Governance
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
}

export interface ExecutionPlane {
  register(...tools: RegisteredTool[]): this
  unregister(name: string): this
  schemas(): ToolSchema[]
  executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent>
}

function stripFrontmatter(content: string): string {
  const s = content.trimStart()
  if (!s.startsWith("---")) return s
  const rest = s.slice(3)
  const end = rest.indexOf("\n---")
  return end >= 0 ? rest.slice(end + 4).trimStart() : s
}

export class LocalExecutionPlane implements ExecutionPlane {
  private tools = new Map<string, RegisteredTool>()

  register(...tools: RegisteredTool[]): this {
    for (const t of tools) this.tools.set(t.schema.name, t)
    return this
  }

  unregister(name: string): this {
    this.tools.delete(name)
    return this
  }

  schemas(): ToolSchema[] {
    return Array.from(this.tools.values()).map(t => t.schema)
  }

  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    const permitted: ToolCall[] = []
    for (const c of calls) {
      if (ctx.governance) {
        ctx.governance.setTime(Date.now())
        const v = ctx.governance.evaluate(c.name, c.arguments)
        if (v.kind === "deny") {
          yield { type: "tool_denied", callId: c.id, toolName: c.name, reason: v.reason ?? "" } as ToolDeniedEvent
          yield { type: "tool_result", callId: c.id, name: c.name, content: `permission denied: ${v.reason ?? ""}`, isError: true, isFatal: false, errorKind: "governance_denied" } as ToolResultEvent
          continue
        }
        if (v.kind === "rate_limited") {
          yield { type: "tool_denied", callId: c.id, toolName: c.name, reason: "rate limited" } as ToolDeniedEvent
          yield { type: "tool_result", callId: c.id, name: c.name, content: "rate limited", isError: true, isFatal: false, errorKind: "recoverable" } as ToolResultEvent
          continue
        }
        if (v.kind === "ask_user") {
          yield { type: "permission_request", callId: c.id, toolName: c.name, arguments: c.arguments, reason: v.reason ?? "" } as PermissionRequestEvent
          yield { type: "tool_result", callId: c.id, name: c.name, content: "awaiting user approval", isError: true, isFatal: false, errorKind: "recoverable" } as ToolResultEvent
          continue
        }
      }
      permitted.push(c)
    }

    const skillCalls = permitted.filter(c => c.name === "skill")
    const memoryCalls = permitted.filter(c => c.name === "memory")
    const knowledgeCalls = permitted.filter(c => c.name === "knowledge")
    const regularCalls = permitted.filter(c => !["skill", "memory", "knowledge"].includes(c.name))

    for (const c of skillCalls) {
      const args = tryParseJson(c.arguments) as Record<string, unknown>
      const name = String(args?.name ?? "")
      const raw = ctx.skillContentMap?.get(name)
      const content = raw != null ? stripFrontmatter(raw) : null
      yield {
        type: "tool_result",
        callId: c.id,
        name: c.name,
        content: content ?? `Skill "${name}" not found.`,
        isError: content == null,
      } as ToolResultEvent
    }

    for (const c of memoryCalls) {
      const args = tryParseJson(c.arguments) as Record<string, unknown>
      const topK = typeof args?.top_k === "number" ? args.top_k : 5
      const entries = (ctx.dreamStore && ctx.agentId)
        ? await ctx.dreamStore.search(ctx.agentId, String(args?.query ?? ""), topK)
        : []
      const content = entries.length
        ? entries.map((e: MemoryEntry) => `[score=${e.score.toFixed(3)}] ${e.text}`).join("\n---\n")
        : "No relevant memories found."
      yield { type: "tool_result", callId: c.id, name: c.name, content, isError: false } as ToolResultEvent
    }

    for (const c of knowledgeCalls) {
      const args = tryParseJson(c.arguments) as Record<string, unknown>
      const topK = typeof args?.top_k === "number" ? args.top_k : 5
      const snippets = ctx.knowledgeSource
        ? await ctx.knowledgeSource.retrieve(String(args?.query ?? ""), topK)
        : []
      const content = snippets.length ? snippets.join("\n---\n") : "No relevant knowledge found."
      yield { type: "tool_result", callId: c.id, name: c.name, content, isError: false } as ToolResultEvent
    }

    for (const call of regularCalls) {
      const registered = this.tools.get(call.name)
      if (!registered) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: `unknown tool: ${call.name}`, isError: true, isFatal: false, errorKind: "recoverable" } as ToolResultEvent
        continue
      }
      try {
        const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
        const output = await registered.execute(args)
        yield { type: "tool_result", callId: call.id, name: call.name, content: String(output), isError: false } as ToolResultEvent
      } catch (err) {
        yield {
          type: "tool_result",
          callId: call.id,
          name: call.name,
          content: String(err),
          isError: true,
          isFatal: Boolean((err as any)?.isFatal),
          errorKind: (err as any)?.errorKind,
        } as ToolResultEvent
      }
    }
  }
}

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}
