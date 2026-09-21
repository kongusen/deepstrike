/**
 * SPC-028-03: names that retain the Canonical prefix for ABI/runtime reasons.
 * New provider-neutral types should use a domain and representation name instead.
 */
export const CANONICAL_PREFIX_ALLOWLIST = [
  "CanonicalAdapterInput",
  "CanonicalCheckpoint",
  "CanonicalCommit",
  "CanonicalKernel",
  "CanonicalKernelHost",
  "CanonicalKernelInput",
  "CanonicalKernelInstance",
  "CanonicalKernelRebuildRequiredError",
  "CanonicalKernelRejectedError",
  "CanonicalMessage",
  "CanonicalMessageBlock",
  "CanonicalPlannedStep",
  "CanonicalPreparation",
  "CanonicalPrepared",
  "CanonicalRejected",
  "CanonicalRenderedContext",
  "CanonicalReplayed",
  "CanonicalRestoreCost",
  "CanonicalRunnerRuntime",
  "CanonicalRunnerRuntimeOptions",
  "CanonicalStopReason",
  "CanonicalToolResult",
  "CanonicalTransition",
  "CanonicalTransitionOptions",
] as const

export type AllowedCanonicalPrefixName = typeof CANONICAL_PREFIX_ALLOWLIST[number]
