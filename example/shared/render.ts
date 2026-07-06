/**
 * Shared stream-event renderer used by L2+ (L1 keeps its own inline copy as a teaching artifact).
 * StreamEvent is a base interface (`type: string`); the concrete events extend it, so we branch on
 * `type` and cast to the matching subinterface.
 */
import type {
  StreamEvent, TextDelta, ToolCallEvent, ToolResultEvent, DoneEvent,
  PermissionRequestEvent, PermissionResolvedEvent,
} from "@deepstrike/sdk"

export function render(event: StreamEvent): void {
  switch (event.type) {
    case "permission_request": {
      const e = event as PermissionRequestEvent
      process.stdout.write(`\n  [⚖ ask_user: ${e.toolName}(${e.arguments.slice(0, 80)}) — ${e.reason}]\n`)
      break
    }
    case "permission_resolved": {
      const e = event as PermissionResolvedEvent
      process.stdout.write(`  [⚖ ${e.approved ? "APPROVED" : "DENIED"} by ${e.responder}${e.reason ? ` — ${e.reason}` : ""}]\n`)
      break
    }
    case "text_delta":
      process.stdout.write((event as TextDelta).delta)
      break
    case "tool_call": {
      const e = event as ToolCallEvent
      const arg = e.arguments ? Object.values(e.arguments)[0] : ""
      process.stdout.write(`\n  [→ ${e.name}(${JSON.stringify(arg)?.slice(0, 120)})]\n`)
      break
    }
    case "tool_result": {
      const e = event as ToolResultEvent
      const preview = e.content.slice(0, 100).replace(/\s+/g, " ")
      process.stdout.write(`  [← ${preview}${e.content.length > 100 ? "…" : ""}]\n`)
      break
    }
    case "done": {
      const e = event as DoneEvent
      process.stdout.write(`\n[done: ${e.status} · ${e.iterations} turns · ~${e.totalTokens} tokens]\n`)
      break
    }
  }
}
