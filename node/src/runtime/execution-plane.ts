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
