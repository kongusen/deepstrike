import { getKernel } from "../../src/kernel.js"
import {
  kernelAction,
  restoreKernelRuntime,
  snapshotKernelRuntime,
} from "../../src/runtime/kernel-step.js"

describe("KernelSnapshot host parity", () => {
  it("restores the operation wire identity and pending provider effect", () => {
    const kernel = getKernel()
    const original = new kernel.KernelRuntime({
      maxTokens: 4096,
      maxTotalTokens: 9_007_199_254_740_993n,
    })
    const pending: never[] = []
    const call = kernelAction(original, pending, {
      kind: "start_run",
      task: { goal: "checkpoint", criteria: [] },
    })
    expect(call.kind).toBe("call_provider")

    const snapshot = snapshotKernelRuntime(original)
    expect(snapshot.initial_policy.max_total_tokens).toBe("9007199254740993")
    const restored = new kernel.KernelRuntime({ maxTokens: 1 })
    restoreKernelRuntime(restored, snapshot)
    expect(snapshotKernelRuntime(restored)).toEqual(snapshot)

    const event = {
      kind: "provider_result",
      effect_id: call.effectId,
      message: { role: "assistant", content: "done", tool_calls: [] },
      observed_input_tokens: 4,
      observed_output_tokens: 1,
      now_ms: 10,
    }
    expect(kernelAction(restored, [], event)).toEqual(kernelAction(original, [], event))
  })
})
