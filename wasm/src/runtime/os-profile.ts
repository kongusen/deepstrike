import type { GovernancePolicy } from "../governance.js"

/** Agent OS SDK runtime profile (Phase 6). */
export type OsProfile = "legacy" | "native"

export interface NativeProfileRequirements {
  osProfile?: OsProfile
  attentionPolicy?: { maxQueueSize?: number }
  governancePolicy?: GovernancePolicy
  governance?: unknown
}

export function isNativeProfile(opts: { osProfile?: OsProfile }): boolean {
  return opts.osProfile === "native"
}

/**
 * Fail-fast validation before a run when `osProfile: "native"`.
 * Native requires in-kernel signal + governance; legacy SDK gates are forbidden.
 */
export function assertNativeProfile(opts: NativeProfileRequirements): void {
  if (!isNativeProfile(opts)) return

  if (!opts.attentionPolicy) {
    throw new Error(
      "osProfile \"native\" requires RuntimeOptions.attentionPolicy (in-kernel signal routing)",
    )
  }
  if (!opts.governancePolicy) {
    throw new Error(
      "osProfile \"native\" requires RuntimeOptions.governancePolicy (in-kernel syscall gate)",
    )
  }
  if (opts.governance) {
    throw new Error(
      "osProfile \"native\" forbids legacy RuntimeOptions.governance; use governancePolicy only",
    )
  }
}

/** Default attention policy for native profile smoke tests. */
export const DEFAULT_NATIVE_ATTENTION_POLICY = { maxQueueSize: 64 }

/** Permissive governance policy for native runs that do not need AskUser. */
export const DEFAULT_NATIVE_GOVERNANCE_POLICY: GovernancePolicy = {
  rules: [{ pattern: "*", action: "allow" }],
}
