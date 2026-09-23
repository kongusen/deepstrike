/** The live kernel observation to host session-event decode crossing. */

import type { BoundaryProtocol } from "./types.js"

export const KERNEL_OBSERVATION_TO_SESSION_EVENT_PROTOCOL: BoundaryProtocol = {
  id: "kernel-observation.to-session-event",
  family: "kernel-to-host",
  direction: "decode",
  fields: {
    forbidden: [],
  },
  lazy: { lazySemantics: "none" },
  adapter: "runtime/kernel-event-log:kernelObservationToSessionEventAtBoundary",
  source: {
    type: "KernelObservationSessionEventRequest",
    layer: "kernel",
    authority: "kernel",
  },
  target: {
    type: "SessionEvent | null",
    layer: "host",
    authority: "host-runtime",
  },
  artifacts: {
    manifest: "contracts/manifests/kernel-observation-to-session-event.json",
  },
  lossiness: "intentional",
}
