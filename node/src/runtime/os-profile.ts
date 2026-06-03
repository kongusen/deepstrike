import type { GovernancePolicy } from "../governance.js"

export type OsProfileId = "native"

export interface NativeOsProfile {
  id: OsProfileId
  attentionPolicy: { maxQueueSize?: number }
  governancePolicy: GovernancePolicy
}

/** Default attention policy for native profile smoke tests. */
export const DEFAULT_NATIVE_ATTENTION_POLICY = { maxQueueSize: 64 }

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
    attentionPolicy: DEFAULT_NATIVE_ATTENTION_POLICY,
    governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
  }
}

/** Assert that a runtime is using a valid native microkernel policy profile. */
export function assertNativeProfile(profile: OsProfileId | NativeOsProfile = "native"): NativeOsProfile {
  const resolved = osProfile(profile)
  if (resolved.id !== "native") {
    throw new Error(`Unsupported OS profile: ${resolved.id}`)
  }
  const validation = validateDeclarativePolicy(resolved.governancePolicy, resolved.attentionPolicy)
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
  attentionPolicy?: { maxQueueSize?: number },
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

  if (attentionPolicy) {
    if (attentionPolicy.maxQueueSize !== undefined) {
      if (typeof attentionPolicy.maxQueueSize !== "number" || attentionPolicy.maxQueueSize <= 0) {
        errors.push("AttentionPolicy maxQueueSize must be a positive integer")
      }
    }
  }

  return {
    valid: errors.length === 0,
    errors,
  }
}
