import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { LLMProvider, StreamEvent } from "../src/types.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"

// `update_plan` is a kernel meta-tool (exposed via `enablePlanTool`), not a plane-registered tool.
// Before the fix the wasm runner had no dispatch branch for it, so every call fell through to the
// execution plane and came back "unknown tool: update_plan" — exposure without execution. The
// node/python runners resolve it as an `update_task` kernel apply; wasm must match.
describe("update_plan meta-tool dispatch", () => {
  it("resolves update_plan as a kernel task update, never via the execution plane", async () => {
    kernelEvents.length = 0
    let providerCalls = 0
    const provider: LLMProvider = {
      async complete() {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream() {
        providerCalls += 1
        if (providerCalls === 1) {
          // wasm provider convention: tool_call arguments are structured objects; the runner
          // JSON.stringifies them once before kernel submission (runner.ts finalToolCalls).
          yield {
            type: "tool_call",
            id: "call_plan",
            name: "update_plan",
            arguments: { plan: ["step a", "step b"], current_step: 1, progress: "started" },
          }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      // Deliberately empty: update_plan must never reach the plane (no "unknown tool" error).
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 2,
      enablePlanTool: true,
    })

    const events: StreamEvent[] = []
    for await (const event of runner.run({ sessionId: "plan-call", goal: "plan the task" })) events.push(event)

    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "call_plan",
      content: "success",
      isError: false,
    }))
    expect(events).not.toContainEqual(expect.objectContaining({
      callId: "call_plan",
      isError: true,
    }))

    const update = kernelEvents.find((e: { kind: string }) => e.kind === "update_task") as
      | { update?: Record<string, unknown> }
      | undefined
    expect(update).toBeDefined()
    expect(update!.update).toEqual(expect.objectContaining({
      plan: ["step a", "step b"],
      current_step: 1,
      progress: "started",
    }))
  })
})
