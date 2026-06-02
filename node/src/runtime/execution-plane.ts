import type {
  ToolCall, ToolResult, ToolSchema, StreamEvent, ToolSuspendEvent, ToolResultEvent, PermissionRequestEvent, ToolDeniedEvent,
  PermissionResponse, PermissionResolvedEvent,
} from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import { isAsyncIterable, normalizeToolChunk, toolChunkText, validateToolArguments } from "../tools/index.js"
import { readSkillFile } from "../skills/loader.js"
import type { DreamStore, MemoryEntry } from "../memory/protocols.js"
import type { KnowledgeSource } from "../knowledge/source.js"

export interface RunContext {
  agentId?: string
  skillDir?: string
  dreamStore?: DreamStore
  knowledgeSource?: KnowledgeSource
  // COMPAT(gov-sdk-gate): legacy SDK-side governance instance, used only when the
  // caller passes the old `governance` option instead of declarative `governancePolicy`.
  // When `kernelGatedCalls` is set (in-kernel gate mode) this field is left undefined.
  // Removable once all callers migrate to the in-kernel gate.
  governance?: {
    setTime?(nowMs: bigint): void
    evaluate(name: string, argsJson: string): { kind: string; reason?: string; retryAfterMs?: number }
  }
  /**
   * In-kernel governance gate mode. When defined (even if empty), the kernel has
   * already decided deny/rate-limit/param-constraint before dispatching these calls,
   * so executeAll skips its own evaluate() pass. Entries are the calls the kernel
   * flagged AskUser (callId → reason); executeAll runs human approval only for those.
   */
  kernelGatedCalls?: Map<string, string>
  onToolSuspend?: (event: ToolSuspendEvent) => Promise<unknown> | unknown
  onPermissionRequest?: (event: PermissionRequestEvent) => Promise<PermissionResponse | boolean> | PermissionResponse | boolean
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
    // Governance pass — denied calls get an immediate tool_result so the kernel always
    // receives a result for every call it dispatched.
    const permitted: ToolCall[] = []
    for (const c of calls) {
      // In-kernel gate mode: the kernel already enforced deny / rate-limit / param
      // constraints before dispatch, so we skip the SDK-side evaluate() entirely.
      // Only calls the kernel flagged AskUser arrive here needing human approval.
      if (ctx.kernelGatedCalls) {
        const reason = ctx.kernelGatedCalls.get(c.id)
        if (reason !== undefined) {
          const request: PermissionRequestEvent = {
            type: "permission_request",
            callId: c.id,
            toolName: c.name,
            arguments: c.arguments,
            reason,
          }
          yield request
          const decision = await resolvePermissionRequest(request, ctx)
          yield {
            type: "permission_resolved",
            callId: c.id,
            toolName: c.name,
            approved: decision.approved,
            responder: decision.responder ?? "host",
            ...(decision.reason ? { reason: decision.reason } : {}),
          } as PermissionResolvedEvent
          if (decision.approved) {
            permitted.push(c)
            continue
          }
          const denyReason = decision.reason ?? reason ?? "permission denied"
          yield { type: "tool_denied", callId: c.id, toolName: c.name, reason: denyReason } as ToolDeniedEvent
          yield {
            type: "tool_result",
            callId: c.id,
            name: c.name,
            content: `permission denied: ${denyReason}`,
            isError: true,
            errorKind: "governance_denied",
          } as ToolResultEvent
          continue
        }
        permitted.push(c)
        continue
      }
      // COMPAT(gov-sdk-gate): legacy full SDK-side gate, active only when no
      // declarative governancePolicy was provided. Removable after migration.
      if (ctx.governance) {
        ctx.governance.setTime?.(BigInt(Date.now()))
        const v = ctx.governance.evaluate(c.name, c.arguments)
        if (v.kind === "deny") {
          yield { type: "tool_denied", callId: c.id, toolName: c.name, reason: v.reason ?? "" } as ToolDeniedEvent
          yield { type: "tool_result", callId: c.id, name: c.name, content: `permission denied: ${v.reason ?? ""}`, isError: true } as ToolResultEvent
          continue
        }
        if (v.kind === "rate_limited") {
          yield { type: "error", message: `rate limited: ${c.name}` } as StreamEvent
          yield { type: "tool_result", callId: c.id, name: c.name, content: "rate limited", isError: true } as ToolResultEvent
          continue
        }
        if (v.kind === "ask_user") {
          const request: PermissionRequestEvent = {
            type: "permission_request",
            callId: c.id,
            toolName: c.name,
            arguments: c.arguments,
            reason: v.reason ?? "",
          }
          yield request

          const decision = await resolvePermissionRequest(request, ctx)
          yield {
            type: "permission_resolved",
            callId: c.id,
            toolName: c.name,
            approved: decision.approved,
            responder: decision.responder ?? "host",
            ...(decision.reason ? { reason: decision.reason } : {}),
          } as PermissionResolvedEvent

          if (decision.approved) {
            permitted.push(c)
            continue
          }

          const reason = decision.reason ?? v.reason ?? "permission denied"
          yield { type: "tool_denied", callId: c.id, toolName: c.name, reason } as ToolDeniedEvent
          yield {
            type: "tool_result",
            callId: c.id,
            name: c.name,
            content: `permission denied: ${reason}`,
            isError: true,
            errorKind: "governance_denied",
          } as ToolResultEvent
          continue
        }
      }
      permitted.push(c)
    }

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

  private async *executeSingle(call: ToolCall, ctx: RunContext): AsyncGenerator<StreamEvent, ToolResult> {
    const registered = this.tools.get(call.name)
    if (!registered) return { callId: call.id, output: `unknown tool: ${call.name}`, isError: true }
    try {
      const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
      const originalArgsStr = JSON.stringify(args)
      const validation = validateToolArguments(registered.schema.parameters, args)
      if (validation.error) return { callId: call.id, output: `invalid arguments: ${validation.error}`, isError: true }
      if (validation.repaired) {
        yield {
          type: "tool_argument_repaired",
          callId: call.id,
          name: call.name,
          originalArguments: originalArgsStr,
          repairedArguments: JSON.stringify(args),
        } as StreamEvent
      }
      const output = await registered.execute(args)
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
          yield { type: "tool_delta", callId: call.id, name: call.name, ...(delta ? { delta } : {}), chunk } as StreamEvent
        }
        return { callId: call.id, output: combined, isError: false }
      }
      return { callId: call.id, output, isError: false }
    } catch (err) {
      return {
        callId: call.id,
        output: String(err),
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
      reason: `permission handler failed: ${String(err)}`,
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
