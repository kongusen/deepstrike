import type { GovernancePolicy } from "./governance.js"

/** Public guardrail declaration. A policy-bearing guardrail lowers into host governance. */
export interface Guardrail {
  name: string
  description?: string
  metadata?: Record<string, unknown>
  /** Optional executable governance policy. A descriptive guardrail without this field is inert. */
  policy?: GovernancePolicy
}
