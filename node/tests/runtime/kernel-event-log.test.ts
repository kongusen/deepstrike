import { categoryForKind, kernelObservationToSessionEvent } from "../../src/runtime/kernel-event-log.js"
import { createRunner, tool } from "./helpers.js"
import { collectText } from "../../src/runtime/runner.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

describe("kernel event log (Phase 5)", () => {
  it("maps observation kinds to OS categories", () => {
    expect(categoryForKind("tool_gated")).toBe("syscall")
    expect(categoryForKind("page_out")).toBe("mm")
    expect(categoryForKind("signal_delivery_disposed")).toBe("ipc")
    expect(categoryForKind("agent_process_changed")).toBe("proc")
    expect(categoryForKind("suspended")).toBe("sched")
  })

  it("kernelObservationToSessionEvent maps kinds (classification derived from kind)", () => {
    const ev = kernelObservationToSessionEvent(
      { kind: "budget_exceeded", turn: 2, budget: "max_turns" },
      2,
    )
    expect(ev).toMatchObject({ kind: "budget_exceeded", budget: "max_turns" })
    expect(categoryForKind(ev!.kind)).toBe("sched")
  })

  it("maps signal_delivery_disposed to ipc session event", () => {
    const ev = kernelObservationToSessionEvent(
      {
        kind: "signal_delivery_disposed",
        turn: 1,
        operation_id: "op-1",
        delivery_id: "delivery-1",
        attempt: 1,
        signal_id: "sig-1",
        disposition: "queue",
        queue_depth: 2,
      },
      1,
    )
    expect(ev).toMatchObject({
      kind: "signal_delivery_disposed",
      delivery_id: "delivery-1",
      disposition: "queue",
      queue_depth: 2,
    })
  })

  it("keeps canonical parent task identity distinct from host session identity", () => {
    const ev = kernelObservationToSessionEvent({
      kind: "agent_process_changed",
      turn: 3,
      agent_id: "child",
      parent_task_id: "root-task",
    }, 3)
    expect(ev).toMatchObject({ parent_task_id: "root-task" })
    expect(ev).not.toHaveProperty("parent_session_id")
  })

  it("governance suspend logs syscall/sched kernel events", async () => {
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
    expect(categoryForKind(gated!.event.kind)).toBe("syscall")
    expect(suspended).toBeDefined()
    expect(categoryForKind(suspended!.event.kind)).toBe("sched")
  })

  it("retains the mm category for the retired page-in side channel", () => {
    // The live memory-tool page-in side channel was retired (strict dynamic control): a memory
    // call no longer emits page_in_requested. The category mapping itself remains (`page_in`
    // stays a valid host-driven event kind for stable pins), so it still classifies as mm.
    expect(categoryForKind("page_in_requested")).toBe("mm")
  })
})
