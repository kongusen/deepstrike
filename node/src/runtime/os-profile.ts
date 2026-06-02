import type { GovernancePolicy } from "../governance.js"

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
