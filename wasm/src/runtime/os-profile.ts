import type { GovernancePolicy } from "../governance.js"

export type OsProfileId = "native"

export interface SignalPolicy {
  queueMax: number
  ttlMs?: number
  deadlineEscalation?: boolean
}

export interface NativeOsProfile {
  id: OsProfileId
  signalPolicy: SignalPolicy
  governancePolicy: GovernancePolicy
}

/** Default signal policy for native profile smoke tests. */
export const DEFAULT_NATIVE_SIGNAL_POLICY: SignalPolicy = { queueMax: 64 }

/** Permissive governance policy for native runs that do not need AskUser. */
export const DEFAULT_NATIVE_GOVERNANCE_POLICY: GovernancePolicy = {
  rules: [{ pattern: "*", action: "allow" }],
}

/** Default restrictive sandbox policy template requiring confirmation for modification/execution. */
export const DEFAULT_SANDBOX_POLICY: GovernancePolicy = {
  rules: [
    { pattern: "read_file", action: "allow" },
    { pattern: "write_file", action: "ask_user" },
    { pattern: "run_command", action: "ask_user" },
    { pattern: "*", action: "deny" },
  ],
}

/** Resolve a named OS profile into concrete kernel-owned policy defaults. */
export function osProfile(profile: OsProfileId | NativeOsProfile = "native"): NativeOsProfile {
  if (typeof profile !== "string") return profile
  if (profile !== "native") throw new Error(`Unsupported OS profile: ${profile}`)
  return {
    id: "native",
    signalPolicy: DEFAULT_NATIVE_SIGNAL_POLICY,
    governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
  }
}

/** Assert that a runtime is using a valid native microkernel policy profile. */
export function assertNativeProfile(profile: OsProfileId | NativeOsProfile = "native"): NativeOsProfile {
  const resolved = osProfile(profile)
  if (resolved.id !== "native") {
    throw new Error(`Unsupported OS profile: ${resolved.id}`)
  }
  const validation = validateDeclarativePolicy(resolved.governancePolicy, resolved.signalPolicy)
  if (!validation.valid) {
    throw new Error(`Invalid native OS profile: ${validation.errors.join("; ")}`)
  }
  return resolved
}

/**
 * Validates the declarative policies statically to prevent runtime crashes when loaded into the microkernel.
 */
export function validateDeclarativePolicy(
  govPolicy?: GovernancePolicy,
  signalPolicy?: SignalPolicy,
): { valid: boolean; errors: string[] } {
  const errors: string[] = []

  if (govPolicy) {
    if (!Array.isArray(govPolicy.rules)) {
      errors.push("GovernancePolicy rules must be an array")
    } else {
      govPolicy.rules.forEach((rule, idx) => {
        if (!rule.pattern || typeof rule.pattern !== "string") {
          errors.push(`Rule[${idx}] pattern is missing or not a string`)
        }
        if (!["allow", "deny", "ask_user"].includes(rule.action)) {
          errors.push(`Rule[${idx}] action '${rule.action}' is invalid. Allowed: allow, deny, ask_user`)
        }
      })
    }
  }

  if (signalPolicy) {
    if (!Number.isInteger(signalPolicy.queueMax) || signalPolicy.queueMax <= 0) {
      errors.push("SignalPolicy queueMax must be a positive integer")
    }
    if (signalPolicy.ttlMs !== undefined && (!Number.isInteger(signalPolicy.ttlMs) || signalPolicy.ttlMs <= 0)) {
      errors.push("SignalPolicy ttlMs must be a positive integer")
    }
    if (signalPolicy.deadlineEscalation !== undefined && typeof signalPolicy.deadlineEscalation !== "boolean") {
      errors.push("SignalPolicy deadlineEscalation must be a boolean")
    }
  }

  return {
    valid: errors.length === 0,
    errors,
  }
}
