import { getKernel } from "../../src/kernel.js"
import { stepKernelV2WithHostEffects } from "../helpers/kernel-v2.js"

// End-to-end through the native module: drive the KernelRuntime ABI directly and
// assert the in-kernel attention policy routes signals (Phase 1 OS-ification).

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

describe("in-kernel attention policy", () => {
  function startedRuntime(maxQueueSize: number) {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "set_attention_policy", max_queue_size: maxQueueSize })
    step(rt, { kind: "start_run", task: { goal: "watch", criteria: [] } })
    return rt
  }

  it("routes a critical signal to an interrupt that drives a provider call", () => {
    const rt = startedRuntime(8)
    const s = step(rt, { kind: "signal", signal: makeSignal("critical", "fire") })
    expect(s.actions).toHaveLength(1)
    expect(s.observations.some(o => o.kind === "signal_disposed" && o.disposition === "interrupt_now")).toBe(true)
  })

  it("queues a normal signal without producing an action", () => {
    const rt = startedRuntime(8)
    const s = step(rt, { kind: "signal", signal: makeSignal("normal", "job") })
    expect(s.actions).toHaveLength(0)
    expect(s.observations.some(o => o.kind === "signal_disposed" && o.disposition === "queue" && o.queue_depth === 1)).toBe(true)
  })

  it("drops a normal signal when the queue is full", () => {
    const rt = startedRuntime(1)
    step(rt, { kind: "signal", signal: makeSignal("normal", "first") })
    const s = step(rt, { kind: "signal", signal: makeSignal("normal", "second") })
    expect(s.observations.some(o => o.kind === "signal_disposed" && o.disposition === "dropped")).toBe(true)
  })

  it("without set_attention_policy uses the default SignalRouter queue (64)", () => {
    const rt = new (getKernel().KernelRuntime)({ maxTokens: 128_000 })
    step(rt, { kind: "start_run", task: { goal: "watch", criteria: [] } })
    const s = step(rt, { kind: "signal", signal: makeSignal("normal", "tick") })
    expect(s.actions).toHaveLength(0)
    expect(s.observations.some(o => o.kind === "signal_disposed" && o.disposition === "queue" && o.queue_depth === 1)).toBe(true)
  })
})
