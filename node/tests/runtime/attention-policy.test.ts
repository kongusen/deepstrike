import { getKernel } from "../../src/kernel.js"

describe("SignalRouter ABI", () => {
  it("accepts lifecycle strings and rejects the retired boolean ABI", () => {
    const router = new (getKernel().SignalRouter)(4)
    const signal = {
      id: crypto.randomUUID(),
      source: "gateway" as const,
      signalType: "alert" as const,
      urgency: "critical" as const,
      summary: "wake now",
      payload: "{}",
      timestampMs: 1,
    }

    expect(router.ingest(signal, "ready")).toBe("run")
    expect(() => router.ingest({ ...signal, id: crypto.randomUUID() }, true as never)).toThrow()
  })

  it("preserves the deadline/coalesce ABI and merges queued entries", () => {
    const router = new (getKernel().SignalRouter)(1)
    const firstId = crypto.randomUUID()
    const first = {
      id: firstId,
      source: "gateway" as const,
      signalType: "event" as const,
      urgency: "normal" as const,
      summary: "batch",
      payload: "{}",
      deadlineMs: 200,
      coalesceKey: "updates",
      coalescedCount: 1,
      timestampMs: 10,
    }
    const second = { ...first, id: crypto.randomUUID(), deadlineMs: 100, timestampMs: 20 }

    expect(router.ingest(first, "running")).toBe("queue")
    expect(router.ingest(second, "running")).toBe("queue")
    const merged = router.next()

    expect(merged).toMatchObject({
      id: firstId,
      deadlineMs: 100,
      coalesceKey: "updates",
      coalescedCount: 2,
    })
    expect(merged).not.toHaveProperty("topic")
    expect(router.next()).toBeNull()
  })
})
