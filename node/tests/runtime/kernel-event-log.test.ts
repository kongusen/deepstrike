import { getKernel } from "../../src/kernel.js"
import { categoryForKind, kernelObservationToSessionEvent } from "../../src/runtime/kernel-event-log.js"
import { createRunner, tool } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

describe("kernel event log (Phase 5)", () => {
  it("maps observation kinds to OS categories", () => {
    expect(categoryForKind("tool_gated")).toBe("syscall")
    expect(categoryForKind("page_out")).toBe("mm")
    expect(categoryForKind("signal_disposed")).toBe("ipc")
    expect(categoryForKind("agent_process_changed")).toBe("proc")
    expect(categoryForKind("suspended")).toBe("sched")
  })

  it("kernelObservationToSessionEvent attaches category", () => {
    const ev = kernelObservationToSessionEvent(
      { kind: "budget_exceeded", turn: 2, budget: "max_turns" },
      2,
    )
    expect(ev).toMatchObject({ kind: "budget_exceeded", category: "sched", budget: "max_turns" })
  })

  it("maps signal_disposed to ipc session event", () => {
    const ev = kernelObservationToSessionEvent(
      {
        kind: "signal_disposed",
        turn: 1,
        signal_id: "sig-1",
        disposition: "queue",
        queue_depth: 2,
      },
      1,
    )
    expect(ev).toMatchObject({
      kind: "signal_disposed",
      category: "ipc",
      disposition: "queue",
      queue_depth: 2,
    })
  })

  it("governance suspend logs syscall/sched kernel events with category", async () => {
    let providerCalls = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        providerCalls += 1
        if (providerCalls === 1) {
          yield { type: "tool_call", id: "call_approval", name: "needs_approval", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("needs_approval", "Needs approval", { type: "object", properties: {} }, () => "ok")],
      {
        maxTurns: 4,
        governancePolicy: { rules: [{ pattern: "needs_approval", action: "ask_user" }] },
        onPermissionRequest: () => ({ approved: true, responder: "test" }),
      },
    )

    await collectText(runner.run({ sessionId: "kernel-log-gov", goal: "go" }))
    const events = await sessionLog.read("kernel-log-gov")
    const gated = events.find(e => e.event.kind === "tool_gated")
    const suspended = events.find(e => e.event.kind === "suspended")
    expect(gated).toBeDefined()
    expect((gated!.event as { category?: string }).category).toBe("syscall")
    expect(suspended).toBeDefined()
    expect((suspended!.event as { category?: string }).category).toBe("sched")
  })

  it("mm-paging session events carry mm category", async () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    const step = (event: Record<string, unknown>) =>
      JSON.parse(rt.step(JSON.stringify({ version: 1, event }))) as {
        observations: Array<{ kind: string }>
      }

    step({ kind: "set_memory_enabled", enabled: true })
    step({ kind: "start_run", task: { goal: "g", criteria: [] } })
    const s = step({
      kind: "provider_result",
      message: {
        role: "assistant",
        content: "",
        tool_calls: [{ id: "m1", name: "memory", arguments: { query: "x", top_k: 1 } }],
      },
    })
    expect(s.observations.some(o => o.kind === "page_in_requested")).toBe(true)
    expect(categoryForKind("page_in_requested")).toBe("mm")
  })
})
