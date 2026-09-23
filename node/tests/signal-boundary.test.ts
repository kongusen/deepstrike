import { signalToKernelEvent } from "../src/runtime/runner.js"

describe("signal host-to-kernel boundary", () => {
  it("projects leased signal data without host lease callbacks", () => {
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
        deadlineMs: 123,
        coalesceKey: "refresh",
        coalescedCount: 3,
      },
    })

    expect(event).toEqual({
      kind: "deliver_signal",
      delivery_id: "delivery-1",
      attempt: 2,
      signal: {
        id: "sig-1",
        source: "gateway",
        signal_type: "event",
        urgency: "high",
        summary: "refresh",
        payload: { goal: "refresh" },
        dedupe_key: "refresh-1",
        recipient: "session-1",
        deadline_ms: 123,
        coalesce_key: "refresh",
        coalesced_count: 3,
        timestamp_ms: expect.any(Number),
      },
    })
  })

  it("uses a stable fallback summary for payloads without a goal", () => {
    expect(signalToKernelEvent({
      signalId: "sig-2",
      deliveryId: "delivery-2",
      deliveryAttempt: 1,
      signal: { source: "custom", signalType: "alert", urgency: "normal", payload: {} },
    }).signal.summary).toBe("signal")
  })
})
