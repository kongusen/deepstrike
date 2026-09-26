import { signalToKernelEvent } from "../src/runtime/runner.js"

describe("signal host-to-kernel boundary", () => {
  it("projects a leased signal straight into the kernel's LogicalSignal vocabulary", () => {
    const event = signalToKernelEvent({
      signalId: "sig-1",
      deliveryId: "delivery-1",
      deliveryAttempt: 2,
      signal: {
        source: "gateway",
        signalType: "event",
        urgency: "high",
        payload: { goal: "refresh" },
        dedupeKey: "refresh-1",
        recipient: "session-1",
        deadlineMs: 10_500,
        coalesceKey: "refresh",
        coalescedCount: 3,
      },
      nowMs: 10_000,
    })

    expect(event).toEqual({
      kind: "deliver_signal",
      delivery_id: "delivery-1",
      attempt: 2,
      signal: {
        signal_id: "sig-1",
        source: "gateway",
        target: { kind: "task", task_id: "session-1" },
        urgency: "high",
        payload: { goal: "refresh" },
        dedupe_key: "refresh-1",
        // The absolute deadline becomes the duration the kernel anchors to its accepted time.
        escalate_after_ms: "500",
      },
    })
    // No host clock on the wire: the kernel stamps admission time itself.
    expect(JSON.stringify(event)).not.toMatch(/timestamp/)
  })

  it("clamps a deadline already in the past to immediate escalation", () => {
    expect(signalToKernelEvent({
      signalId: "sig-late",
      deliveryId: "delivery-late",
      deliveryAttempt: 1,
      signal: { source: "cron", signalType: "job", urgency: "low", payload: {}, deadlineMs: 5 },
      nowMs: 1_000,
    }).signal.escalate_after_ms).toBe("0")
  })

  it("sends a host note as its own plain-text payload, not via an id convention", () => {
    const event = signalToKernelEvent({
      signalId: "sig-note",
      deliveryId: "any-delivery-id",
      deliveryAttempt: 1,
      signal: { source: "custom", signalType: "event", urgency: "normal", payload: { goal: "ignored" } },
      note: "check the build output",
    })
    expect(event.signal.payload).toBe("check the build output")
    expect(event.signal.target).toEqual({ kind: "operation" })
  })
})
