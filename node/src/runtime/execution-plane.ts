import type {
  ToolCall, ToolResult, ToolSchema, StreamEvent, ToolSuspendEvent, ToolResultEvent, ToolAuditFailedEvent,
  PermissionRequestEvent, ToolDeniedEvent, PermissionResponse, PermissionResolvedEvent,
} from "../types.js"
import type { RegisteredTool, ToolExecContext } from "../tools/index.js"
import { isAsyncIterable, maybeWarnFailureShapedChunk, normalizeToolChunk, toolChunkText, validateToolArguments } from "../tools/index.js"
import { formatToolError } from "../tools/errors.js"
import { readSkillFile } from "../skills/loader.js"
import type { DreamStore, MemoryScope } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"
import { LargeResultSpool } from "./large-result-spool.js"
import type { OperationContext } from "./reliability.js"

export interface RunContext {
  /** Immutable identity, deadline, and cancellation boundary for this operation. */
  operation?: OperationContext
  agentId?: string
  memoryScope?: MemoryScope
  skillDir?: string
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
  resultSpool?: LargeResultSpool
  /** M3/G4 worktree isolation: the working directory a sub-agent's tools should run in (the git
   *  worktree created for an `isolation: "worktree"` node). Injected by `WorktreeExecutionPlane`; a
   *  cwd-aware execution plane / tool reads it to scope filesystem + subprocess work. Undefined ⇒
   *  the plane's own default cwd. */
  cwd?: string
}

export interface ExecutionPlane {
  register(...tools: RegisteredTool[]): this
  unregister(name: string): this
  schemas(): ToolSchema[]
  /**
   * Execute a batch of calls. Yields StreamEvents during execution.
   * Guarantees exactly one `tool_result` event per call in `calls`.
   * The runner collects those events to build ToolResult[] for the kernel.
   */
  executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent>
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

    const skillCalls     = permitted.filter(c => c.name === "skill")
    const memoryCalls    = permitted.filter(c => c.name === "memory")
    const knowledgeCalls = permitted.filter(c => c.name === "knowledge")
    const regularCalls   = permitted.filter(c => !["skill", "memory", "knowledge"].includes(c.name))

    for (const c of skillCalls) {
      const name = String((tryParseJson(c.arguments) as Record<string, unknown>)?.name ?? "")
      const content = ctx.skillDir ? await readSkillFile(ctx.skillDir, name) : null
      yield { type: "tool_result", callId: c.id, name: c.name, content: content ?? `Skill "${name}" not found.`, isError: !content } as ToolResultEvent
    }

    for (const c of memoryCalls) {
      const args = tryParseJson(c.arguments) as Record<string, unknown>
      const topK = typeof args?.top_k === "number" ? args.top_k : 5
      const entries = (ctx.dreamStore && ctx.agentId && ctx.memoryScope)
        ? await ctx.dreamStore.search(ctx.agentId, {
          scope: ctx.memoryScope,
          query: String(args?.query ?? ""),
          top_k: topK,
          kinds: [],
        })
        : []
      const content = entries.length
        ? entries.map(e => `[memory record_id=${e.record.record_id} trust=${e.record.provenance.trust} score=${e.score.toFixed(3)}] ${e.record.content}`).join("\n---\n")
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

    // Regular tools run concurrently; intermediate stream events (delta, suspend) are
    // yielded as they arrive; the final tool_result is emitted when each call finishes.
    if (regularCalls.length > 0) {
      type Task = {
        call: ToolCall
        gen: AsyncGenerator<StreamEvent, ToolResult>
        pending: Promise<IteratorResult<StreamEvent, ToolResult>>
      }
      const active: Task[] = regularCalls.map(call => {
        const gen = this.executeSingle(call, ctx)
        return { call, gen, pending: gen.next() }
      })

      while (active.length > 0) {
        const { index, result } = await Promise.race(
          active.map((task, i) => task.pending.then(r => ({ index: i, result: r }))),
        )
        const task = active[index]
        if (result.done) {
          const r = result.value
          yield {
            type: "tool_result",
            callId: r.callId,
            name: task.call.name,
            content: r.output,
            isError: r.isError,
            isFatal: r.isFatal,
            errorKind: r.errorKind,
          } as ToolResultEvent
          active.splice(index, 1)
          continue
        }
        yield result.value
        task.pending = task.gen.next()
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

  private async *executeSingle(call: ToolCall, ctx: RunContext): AsyncGenerator<StreamEvent, ToolResult> {
    const spooledContent = await this.tryReadSpooledArgument(call, ctx)
    if (spooledContent !== null) {
      return { callId: call.id, output: spooledContent, isError: false }
    }

    const registered = this.tools.get(call.name)
    if (!registered) return { callId: call.id, output: `unknown tool: ${call.name}`, isError: true }
    // `audit` failure buffer is hoisted above the try-block so the catch path can flush any
    // best-effort failures recorded before the main throw.
    const auditFailures: Array<{ label: string; error: string }> = []
    const callCtx: ToolExecContext = {
      ...(ctx.operation !== undefined ? { operation: ctx.operation } : {}),
      ...(ctx.cwd !== undefined ? { cwd: ctx.cwd } : {}),
      audit: async (label, fn) => {
        try { await fn() }
        catch (err) { auditFailures.push({ label, error: formatToolError(err) }) }
      },
    }
    try {
      const rawArgs = JSON.parse(call.arguments || "{}") as Record<string, unknown>
      const originalArgsStr = JSON.stringify(rawArgs)
      // validation.args, not rawArgs, from here on: a oneOf/anyOf ROOT accepts a repaired probe
      // CLONE — the original reference never sees those repairs (auto-casts, strips, defaults).
      const validation = validateToolArguments(registered.schema.parameters, rawArgs)
      if (validation.error) return { callId: call.id, output: `invalid arguments: ${validation.error}`, isError: true }
      if (validation.repaired) {
        yield {
          type: "tool_argument_repaired",
          callId: call.id,
          name: call.name,
          originalArguments: originalArgsStr,
          repairedArguments: JSON.stringify(validation.args),
        } as StreamEvent
      }
      // M3/G4: pass the run context (incl. `cwd`) so cwd-aware tools scope their work to the
      // sub-agent's worktree. `RunContext` is structurally assignable to the tool's `ToolExecContext`.
      // The per-call `audit` helper (above) layers best-effort side-effect handling on top.
      const output = await registered.execute(validation.args, callCtx)
      if (isAsyncIterable(output)) {
        let combined = ""
        const iterator = output[Symbol.asyncIterator]()
        let resumeValue: unknown
        while (true) {
          const next = await iterator.next(resumeValue)
          resumeValue = undefined
          if (next.done) break
          const chunk = normalizeToolChunk(next.value)
          if (chunk.type === "suspend") {
            const event: ToolSuspendEvent = {
              type: "tool_suspend", callId: call.id, name: call.name,
              suspensionId: chunk.suspensionId,
              ...(chunk.payload ? { payload: chunk.payload } : {}),
            }
            yield event
            if (!ctx.onToolSuspend) {
              return { callId: call.id, output: `tool suspended without resume handler: ${chunk.suspensionId}`, isError: true }
            }
            resumeValue = await ctx.onToolSuspend(event)
            continue
          }
          const delta = toolChunkText(next.value)
          combined += delta
          if (delta) maybeWarnFailureShapedChunk(call.name, delta)
          yield { type: "tool_delta", callId: call.id, name: call.name, ...(delta ? { delta } : {}), chunk } as StreamEvent
        }
        for (const f of auditFailures) {
          yield { type: "tool_audit_failed", callId: call.id, name: call.name, label: f.label, error: f.error } as ToolAuditFailedEvent
        }
        return { callId: call.id, output: combined, isError: false }
      }
      for (const f of auditFailures) {
        yield { type: "tool_audit_failed", callId: call.id, name: call.name, label: f.label, error: f.error } as ToolAuditFailedEvent
      }
      return { callId: call.id, output, isError: false }
    } catch (err) {
      // Audit failures recorded before the main throw are still informational; surface them.
      for (const f of auditFailures) {
        yield { type: "tool_audit_failed", callId: call.id, name: call.name, label: f.label, error: f.error } as ToolAuditFailedEvent
      }
      return {
        callId: call.id,
        output: formatToolError(err),
        isError: true,
        isFatal: Boolean((err as any)?.isFatal),
        errorKind: (err as any)?.errorKind,
      }
    }
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
    return normalizePermissionDecision(await ctx.onPermissionRequest(request))
  } catch (err) {
    return {
      approved: false,
      responder: "permission_handler",
      reason: `permission handler failed: ${formatToolError(err)}`,
    }
  }
}

function normalizePermissionDecision(value: PermissionResponse | boolean): PermissionResponse {
  if (typeof value === "boolean") return { approved: value, responder: "host" }
  return {
    approved: Boolean(value.approved),
    responder: value.responder ?? "host",
    ...(value.reason ? { reason: value.reason } : {}),
  }
}
