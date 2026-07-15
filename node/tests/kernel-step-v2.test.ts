import {
  KERNEL_ABI_VERSION,
  kernelAction,
  type KernelObservation,
  type KernelRuntimeHandle,
} from "../src/runtime/kernel-step.js"

class FakeKernel implements KernelRuntimeHandle {
  readonly inputs: Array<Record<string, unknown>> = []
  private readonly steps: Array<Record<string, unknown>>

  constructor(...steps: Array<Record<string, unknown>>) {
    this.steps = steps
  }

  step(inputJson: string): string {
    this.inputs.push(JSON.parse(inputJson) as Record<string, unknown>)
    const step = this.steps.shift()
    if (!step) throw new Error("missing fake step")
    return JSON.stringify(step)
  }

  isTerminal(): boolean { return false }
  turn(): number { return 0 }
  recoveryContentBytes(): number { return 1024 }
  render(): never { throw new Error("unused") }
  drainNewMessages(): never[] { return [] }
  preservedRefs(): string[] { return [] }
}

const step = (action: Record<string, unknown>, faults: unknown[] = []) => ({
  version: 2,
  operation_id: "op",
  input_event_id: "event",
  step_seq: 1,
  actions: [action],
  observations: [],
  faults,
})

test("kernel adapter sends ABI v2 identities and preserves effect ids", () => {
  const runtime = new FakeKernel(
    step({
      effect_id: "effect-spool",
      causation_id: "event-1",
      kind: "spool_large_result",
      call_id: "call-1",
      tool: "search",
      output: "full output",
      original_size: 11,
      preview_size: 4,
    }),
    step({ effect_id: "effect-done", causation_id: "event-2", kind: "done", result: {} }),
  )
  const observations: KernelObservation[] = []

  const action = kernelAction(runtime, observations, { kind: "start_run" })
  expect(action).toEqual({
    kind: "spool_large_result",
    effectId: "effect-spool",
    callId: "call-1",
    tool: "search",
    output: "full output",
    originalSize: 11,
    previewSize: 4,
  })
  kernelAction(runtime, observations, {
    kind: "large_result_spool_result",
    effect_id: action.effectId,
    spool_ref: "spool://call-1",
  })

  expect(KERNEL_ABI_VERSION).toBe(2)
  expect(runtime.inputs[0]?.version).toBe(2)
  expect(runtime.inputs[0]?.operation_id).toBe(runtime.inputs[1]?.operation_id)
  expect(runtime.inputs[0]?.event_id).not.toBe(runtime.inputs[1]?.event_id)
  expect(typeof runtime.inputs[0]?.observed_at_ms).toBe("number")
})

test("kernel adapter rejects structured kernel faults", () => {
  const runtime = new FakeKernel({
    ...step({ effect_id: "unused", causation_id: "event", kind: "done", result: {} }),
    actions: [],
    faults: [{ code: "invalid_lifecycle", message: "cannot start twice" }],
  })

  expect(() => kernelAction(runtime, [], { kind: "start_run" })).toThrow(
    "invalid_lifecycle: cannot start twice",
  )
})
