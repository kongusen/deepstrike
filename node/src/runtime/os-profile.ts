import type { GovernancePolicy } from "../governance.js"

/** Default attention policy for native profile smoke tests. */
export const DEFAULT_NATIVE_ATTENTION_POLICY = { maxQueueSize: 64 }

/** Permissive governance policy for native runs that do not need AskUser. */
export const DEFAULT_NATIVE_GOVERNANCE_POLICY: GovernancePolicy = {
  rules: [{ pattern: "*", action: "allow" }],
}
