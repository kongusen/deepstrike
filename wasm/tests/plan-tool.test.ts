import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { LLMProvider, StreamEvent } from "../src/types.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"

// `update_plan` is a kernel syscall (exposed via `enablePlanTool`): the kernel decodes and applies it
// from the provider result. The host must neither route it to the execution plane nor re-apply it
// as its own `update_task` — the model's plan is the model's.
describe("update_plan meta-tool dispatch", () => {
  it("leaves update_plan to the kernel: no plane call, no host update_task", async () => {
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

    expect(events.filter(event => event.type === "error")).toEqual([])
    expect(events).not.toContainEqual(expect.objectContaining({ type: "tool_result", callId: "call_plan" }))
    expect(providerCalls).toBe(2)
    const hostUpdates = kernelEvents.filter((e: { kind: string; command?: { kind?: string } }) =>
      e.kind === "host_control" && e.command?.kind === "update_task")
    expect(hostUpdates).toEqual([])
  })
})
