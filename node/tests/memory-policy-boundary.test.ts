import { memoryPolicyToKernel } from "../src/runtime/runner.js"

describe("memory policy host-to-kernel boundary", () => {
  it("projects declarative memory policy into the kernel wire shape", () => {
    expect(memoryPolicyToKernel({
      staleWarningDays: 7,
      retrievalTopK: 4,
      validationEnabled: true,
      maxContentBytes: 4096,
      maxNameLength: 80,
      promotionRecallThreshold: 3,
    })).toEqual({
      stale_warning_days: 7,
      retrieval_top_k: 4,
      validation_enabled: true,
      max_content_bytes: 4096,
      max_name_length: 80,
      promotion_recall_threshold: 3,
    })
  })

  it("omits unspecified policy fields", () => {
    expect(memoryPolicyToKernel({ retrievalTopK: 2 })).toEqual({ retrieval_top_k: 2 })
  })

  it("rejects fields outside the declared policy", () => {
    expect(() => memoryPolicyToKernel({ retrievalTopK: 2, storagePath: ".memory" } as never)).toThrow(
      "unknown memory policy field(s): storagePath",
    )
  })
})
