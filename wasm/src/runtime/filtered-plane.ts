import type {
  ToolCall, ToolSchema, StreamEvent, ToolResultEvent, ToolDeniedEvent,
} from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"

const DEFAULT_META_TOOLS = new Set(["skill", "memory", "knowledge", "update_plan"])

/** Wraps an execution plane, allowing only manifest-permitted tool IDs (+ meta-tools). */
export class FilteredExecutionPlane implements ExecutionPlane {
  constructor(
    private readonly inner: ExecutionPlane,
    private readonly permittedIds: Set<string>,
    private readonly metaTools: Set<string> = DEFAULT_META_TOOLS,
  ) {}

  register(...tools: RegisteredTool[]): this {
    this.inner.register(...tools)
    return this
  }

  unregister(name: string): this {
    this.inner.unregister(name)
    return this
  }

  schemas(): ToolSchema[] {
    return this.inner.schemas().filter(s => this.permittedIds.has(s.name) || this.metaTools.has(s.name))
  }

  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    const permitted: ToolCall[] = []
    for (const call of calls) {
      if (this.metaTools.has(call.name) || this.permittedIds.has(call.name)) {
        permitted.push(call)
        continue
      }
      const reason = `capability not permitted for sub-agent: ${call.name}`
      yield { type: "tool_denied", callId: call.id, toolName: call.name, reason } as ToolDeniedEvent
      yield {
        type: "tool_result",
        callId: call.id,
        name: call.name,
        content: reason,
        isError: true,
        errorKind: "governance_denied",
      } as ToolResultEvent
    }
    if (permitted.length > 0) {
      yield* this.inner.executeAll(permitted, ctx)
    }
  }
}
