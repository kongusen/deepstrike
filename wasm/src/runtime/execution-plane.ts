import type {
  ToolCall, ToolSchema, StreamEvent, ToolResultEvent, ToolAuditFailedEvent,
  PermissionRequestEvent, ToolDeniedEvent, PermissionResponse, PermissionResolvedEvent,
} from "../types.js"
import type { RegisteredTool, ToolExecContext } from "../tools/index.js"
import type { DreamStore, MemoryScope } from "../memory/index.js"
import type { KnowledgeSource } from "../knowledge/index.js"
import { LargeResultSpool } from "./large-result-spool.js"
import { formatToolError } from "../tools/errors.js"

export interface ToolSuspendEvent {
  type: "tool_suspend"
  callId: string
  name: string
  suspensionId: string
  payload?: unknown
}

export interface RunContext {
  agentId?: string
  memoryScope?: MemoryScope
  skillContentMap?: Map<string, string>
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  resultSpool?: LargeResultSpool
  /** M3/G4: working directory a tool should run in. WASM has no filesystem, so this is carried for
   *  tool-ABI parity with Node/Python rather than consumed by a worktree plane. */
  cwd?: string
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
    const permitted = calls

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
      const entries = (ctx.dreamStore && ctx.agentId && ctx.memoryScope)
        ? await ctx.dreamStore.search(ctx.agentId, { scope: ctx.memoryScope, query: String(args?.query ?? ""), top_k: topK, kinds: [] })
        : []
      const content = entries.length
        ? entries.map(e => `[memory record_id=${e.record.record_id} score=${e.score.toFixed(3)}] ${e.record.content}`).join("\n---\n")
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
      const spooledContent = await this.tryReadSpooledArgument(call, ctx)
      if (spooledContent !== null) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: spooledContent, isError: false } as ToolResultEvent
        continue
      }

      const registered = this.tools.get(call.name)
      if (!registered) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: `unknown tool: ${call.name}`, isError: true, isFatal: false, errorKind: "recoverable" } as ToolResultEvent
        continue
      }
      // Per-call `audit` helper: failures collected here are surfaced as `tool_audit_failed`
      // events rather than flipping the main tool result to `isError: true`.
      const auditFailures: Array<{ label: string; error: string }> = []
      const callCtx: ToolExecContext = {
        ...(ctx.cwd !== undefined ? { cwd: ctx.cwd } : {}),
        audit: async (label, fn) => {
          try { await fn() }
          catch (err) { auditFailures.push({ label, error: formatToolError(err) }) }
        },
      }
      try {
        const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
        // M3/G4: pass the run context (incl. `cwd`, `audit`) for tool-ABI parity with Node/Python.
        const output = await registered.execute(args, callCtx)
        for (const f of auditFailures) {
          yield { type: "tool_audit_failed", callId: call.id, name: call.name, label: f.label, error: f.error } as ToolAuditFailedEvent
        }
        yield { type: "tool_result", callId: call.id, name: call.name, content: String(output), isError: false } as ToolResultEvent
      } catch (err) {
        for (const f of auditFailures) {
          yield { type: "tool_audit_failed", callId: call.id, name: call.name, label: f.label, error: f.error } as ToolAuditFailedEvent
        }
        yield {
          type: "tool_result",
          callId: call.id,
          name: call.name,
          content: formatToolError(err),
          isError: true,
          isFatal: Boolean((err as any)?.isFatal),
          errorKind: (err as any)?.errorKind,
        } as ToolResultEvent
      }
    }
  }

  private async tryReadSpooledArgument(call: ToolCall, ctx: RunContext): Promise<string | null> {
    const isReadTool = ["read", "read_file", "view_file", "read_spooled_result"].includes(call.name)
    if (!isReadTool) return null

    try {
      const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
      for (const val of Object.values(args)) {
        if (typeof val === "string" && (val.startsWith(".spool/") || val.includes("/.spool/"))) {
          const spool = ctx.resultSpool ?? new LargeResultSpool()
          const content = await spool.readSpooledResult(val)
          return content
        }
      }
    } catch {
      // Ignore errors
    }
    return null
  }
}

function tryParseJson(s: string): unknown {
  try { return JSON.parse(s) } catch { return null }
}

export async function resolvePermissionRequest(request: PermissionRequestEvent, ctx: RunContext): Promise<PermissionResponse> {
  if (!ctx.onPermissionRequest) {
    return {
      approved: false,
      responder: "policy_gate",
      reason: "no permission handler configured",
    }
  }

  try {
    return normalizePermissionResponse(await ctx.onPermissionRequest(request))
  } catch (err) {
    return {
      approved: false,
      responder: "permission_handler",
      reason: `permission handler failed: ${formatToolError(err)}`,
    }
  }
}

function normalizePermissionResponse(value: PermissionResponse | boolean): PermissionResponse {
  if (typeof value === "boolean") return { approved: value, responder: "host" }
  return {
    approved: Boolean(value.approved),
    responder: value.responder ?? "host",
    ...(value.reason ? { reason: value.reason } : {}),
  }
}
