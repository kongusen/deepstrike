import { getKernel } from "../../src/kernel.js"
import { governancePolicyToKernelEvent } from "../../src/governance.js"
import { createRunner, tool } from "./helpers.js"
import type { LLMProvider } from "../../src/types.js"
import { stepKernelV2WithHostEffects } from "../helpers/kernel-v2.js"

function step(rt: { step(json: string): string }, event: Record<string, unknown>) {
  return stepKernelV2WithHostEffects(rt as never, event) as {
    actions: Array<Record<string, unknown>>
    observations: Array<{ kind: string; reason?: string; call_id?: string; tool?: string }>
  }
}

describe("scheduler lifecycle (Phase 2)", () => {
  it("ask_user suspends then resume executes approved tool", async () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, governancePolicyToKernelEvent({
      rules: [{ pattern: "needs_approval", action: "ask_user" }],
    }))
    step(rt, { kind: "start_run", task: { goal: "run", criteria: [] } })
    const proposed = step(rt, {
      kind: "provider_result",
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "call_1", name: "needs_approval", arguments: {} }],
      },
    })
    expect(proposed.actions[0]?.kind).toBe("request_approval")
    expect(proposed.observations.some(o => o.kind === "suspended")).toBe(true)

    const resumed = step(rt, {
      kind: "approval_result",
      approved_calls: ["call_1"],
      denied_calls: [],
    })
    expect(resumed.actions[0]?.kind).toBe("execute_tool")
    expect(resumed.observations.some(o => o.kind === "resumed")).toBe(true)
  })

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
