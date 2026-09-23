/** Composite configure_run policy projections, represented as typed child adapters. */

import type { BoundaryProtocol } from "./types.js"

export const CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "run-config.configure-run",
  family: "host-to-kernel",
  direction: "project",
  fields: { forbidden: [] },
  lazy: { lazySemantics: "none" },
  adapters: [
    {
      adapter: "governance:governancePolicyToKernelEvent",
      source: { type: "GovernancePolicy", layer: "host", authority: "host-runtime" },
      target: { type: "KernelGovernancePolicy", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["defaultAction", "rules", "vetoes", "rateLimits", "constraints", "surfaceDeniedInSystem"],
        derived: ["kind", "default_action", "rules", "vetoed_tools", "rate_limits", "constraints"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/context-policy:normalizeContextPolicy",
      source: { type: "ContextPolicy", layer: "host", authority: "host-runtime" },
      target: { type: "ContextPolicyWire", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["pressureThresholds", "targetAfterCompress", "preserveRecentTurns", "renewalCarryover", "collapseOldAssistantNarration", "idleMicroCompactMinutes"],
        derived: ["pressure_thresholds_ppm", "target_after_compress_ppm", "preserve_recent_turns", "renewal_carryover_ppm", "collapse_old_assistant_narration", "idle_micro_compact_minutes"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/runner:kernelReliabilityToKernel",
      source: { type: "KernelReliabilityOptions", layer: "host", authority: "host-runtime" },
      target: { type: "KernelReliabilityPolicy", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["providerRecoveryAttempts", "outputRecoveryAttempts", "maxInputBytes"],
        derived: ["provider_recovery_attempts", "output_recovery_attempts", "max_input_bytes"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/os-profile:signalPolicyToKernel",
      source: { type: "SignalPolicy", layer: "host", authority: "host-runtime" },
      target: { type: "KernelSignalPolicy", layer: "kernel", authority: "kernel" },
      fields: {
        drops: ["queueMax", "ttlMs", "deadlineEscalation"],
        derived: ["queue_max", "ttl_ms", "deadline_escalation"],
        forbidden: [],
      },
    },
  ],
  lossiness: "intentional",
}
