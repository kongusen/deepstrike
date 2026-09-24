/** Memory policy projection used by the live kernel configuration path. */

import type { BoundaryProtocol } from "./types.js"

export const MEMORY_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "memory.host-to-kernel",
  family: "host-to-kernel",
  direction: "project",
  fields: {
    renames: {
      staleWarningDays: "stale_warning_days",
      retrievalTopK: "retrieval_top_k",
      validationEnabled: "validation_enabled",
      maxContentBytes: "max_content_bytes",
      maxNameLength: "max_name_length",
      promotionRecallThreshold: "promotion_recall_threshold",
    },
    forbidden: [],
  },
  lazy: { lazySemantics: "none" },
  adapter: "runtime/runner:memoryPolicyToKernel",
  source: { type: "MemoryPolicy", layer: "host", authority: "host-runtime" },
  target: { type: "KernelMemoryPolicy", layer: "kernel", authority: "kernel" },
  artifacts: { manifest: "contracts/manifests/memory-host-to-kernel.json" },
  lossiness: "intentional",
  validation: {
    mode: "behavioral-tests",
    reason: "Memory policy boundary tests cover snake-case projection and unknown-field rejection.",
    testRefs: [{ path: "node/tests/memory-policy-boundary.test.ts", selectors: ["memory policy host-to-kernel boundary"] }],
  },
}
