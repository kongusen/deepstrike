/** Leased signal data projection used by the live runner signal trap. */

import type { BoundaryProtocol } from "./types.js"

export const SIGNAL_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "signal.host-to-kernel",
  family: "host-to-kernel",
  direction: "project",
  fields: {
    envelope: { source: ["signalId", "deliveryId", "deliveryAttempt", "signal", "note", "nowMs"] },
    nested: [
      {
        source: "signal",
        target: "signal",
        kind: "project",
        note: "Lowered straight into the kernel LogicalSignal: recipient→target, deadlineMs→escalate_after_ms (measured against nowMs), note→plain-text payload. signalType / coalesceKey / coalescedCount are signal-source concepts the canonical wire deliberately does not carry (§5n).",
      },
    ],
    forbidden: [],
  },
  lazy: { lazySemantics: "none" },
  adapter: "runtime/runner:signalToKernelEvent",
  source: { type: "KernelSignalDeliveryRequest", layer: "host", authority: "host-runtime" },
  target: { type: "KernelSignalDeliveryEvent", layer: "kernel", authority: "kernel" },
  artifacts: { manifest: "contracts/manifests/signal-host-to-kernel.json" },
  lossiness: "intentional",
  validation: {
    mode: "behavioral-tests",
    reason: "Signal delivery tests cover leased acknowledgement and the kernel event projection shape.",
    testRefs: [{ path: "node/tests/signal-boundary.test.ts", selectors: ["signal host-to-kernel boundary"] }],
  },
}
