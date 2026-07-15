import { getKernel } from "../../src/kernel.js"
import { stepKernelV2WithHostEffects } from "../helpers/kernel-v2.js"

// End-to-end through the native module: drive the KernelRuntime ABI directly and
// assert the in-kernel signal policy routes signals.

interface StepObservation {
  kind: string
  disposition?: string
  queue_depth?: number
}

function step(rt: { step(json: string): string }, event: Record<string, unknown>): { actions: unknown[]; observations: StepObservation[] } {
  return stepKernelV2WithHostEffects(rt as never, event) as { actions: unknown[]; observations: StepObservation[] }
}

function makeSignal(urgency: string, summary: string) {
  return {
    id: crypto.randomUUID(),
    source: "gateway",
    signal_type: "alert",
    urgency,
    summary,
    payload: {},
    timestamp_ms: Date.now(),
  }
}

function deliverSignal(urgency: string, summary: string, deliveryId = crypto.randomUUID()) {
  return {
    kind: "deliver_signal",
    delivery_id: deliveryId,
    attempt: 1,
    signal: makeSignal(urgency, summary),
  }
}

describe("in-kernel signal policy", () => {
  function startedRuntime(queueMax: number) {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, {
      kind: "set_signal_policy",
      policy: { version: 1, queue_max: queueMax },
    })
    step(rt, { kind: "start_run", task: { goal: "watch", criteria: [] } })
    return rt
  }

  it("routes a critical signal to an interrupt that drives a provider call", () => {
    const rt = startedRuntime(8)
    const s = step(rt, deliverSignal("critical", "fire", "delivery-critical"))
    expect(s.actions).toHaveLength(1)
    expect(s.observations.some(o => o.kind === "signal_delivery_disposed" && o.disposition === "interrupt_now")).toBe(true)
  })

  it("queues a normal signal without producing an action", () => {
    const rt = startedRuntime(8)
    const s = step(rt, deliverSignal("normal", "job"))
    expect(s.actions).toHaveLength(0)
    expect(s.observations.some(o => o.kind === "signal_delivery_disposed" && o.disposition === "queue" && o.queue_depth === 1)).toBe(true)
  })

  it("drops a normal signal when the queue is full", () => {
    const rt = startedRuntime(1)
    step(rt, deliverSignal("normal", "first"))
    const s = step(rt, deliverSignal("normal", "second"))
    expect(s.observations.some(o => o.kind === "signal_delivery_disposed" && o.disposition === "dropped")).toBe(true)
  })

  it("without set_signal_policy uses the default SignalRouter queue (64)", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "watch", criteria: [] } })
    const s = step(rt, deliverSignal("normal", "tick"))
    expect(s.actions).toHaveLength(0)
    expect(s.observations.some(o => o.kind === "signal_delivery_disposed" && o.disposition === "queue" && o.queue_depth === 1)).toBe(true)
  })

  it("SignalRouter accepts lifecycle strings and rejects the retired boolean ABI", () => {
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

  it("SignalRouter preserves the deadline/coalesce ABI and merges queued entries", () => {
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
