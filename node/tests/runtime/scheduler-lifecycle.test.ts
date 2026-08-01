import { createRunner, tool } from "./helpers.js"
import type { LLMProvider } from "../../src/types.js"

describe("scheduler lifecycle (Phase 2)", () => {
  it("runner resolves governancePolicy suspend and runs approved tool", async () => {
    let providerCalls = 0
    const provider: LLMProvider = {
      async complete() { return { role: "assistant", content: "done", toolCalls: [] } },
      async *stream() {
        providerCalls += 1
        if (providerCalls === 1) {
          yield { type: "tool_call", id: "call_approval", name: "needs_approval", arguments: {} }
        } else {
          yield { type: "text_delta", delta: "done" }
        }
      },
    }
    let executed = false
    const { runner } = createRunner(
      provider,
      [tool("needs_approval", "Needs approval", { type: "object", properties: {} }, () => {
        executed = true
        return "ok"
      })],
      {
        maxTurns: 3,
        governancePolicy: { rules: [{ pattern: "needs_approval", action: "ask_user" }] },
        onPermissionRequest: req => ({
          approved: req.toolName === "needs_approval",
          responder: "test",
        }),
      },
    )

    const events = []
    for await (const evt of runner.run({ sessionId: "sched-lifecycle", goal: "go" })) {
      events.push(evt)
    }

    expect(executed).toBe(true)
    expect(events).toContainEqual(expect.objectContaining({
      type: "permission_request",
      callId: "call_approval",
    }))
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "call_approval",
      content: "ok",
    }))
  })
})
