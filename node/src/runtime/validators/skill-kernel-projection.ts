/**
 * Generated runtime validator for skill host-to-kernel boundary crossing.
 *
 * This validator enforces the boundary protocol at runtime, catching violations
 * that TypeScript cannot prevent (e.g., object spread leaking forbidden fields).
 *
 * Generated from: contracts/protocols/skill-host-to-kernel.ts
 * DO NOT EDIT BY HAND - regenerate with: npm run contracts:check
 */

import type { KernelSkillMetadata } from "../runtime/kernel-step.js"

/**
 * Forbidden fields that MUST NOT cross the skill host-to-kernel boundary.
 * These are security-critical encapsulation policies.
 */
const FORBIDDEN_FIELDS = [
  "provider_credentials",
  "activation_authority",
  "storage_backend",
  "user_storage_path",
  "source_adapter",
] as const

/**
 * Required fields that MUST be preserved across the boundary.
 */
const REQUIRED_PRESERVED_FIELDS = ["name", "description"] as const

/**
 * Allowed target fields (union of preserves + renames targets).
 */
const ALLOWED_TARGET_FIELDS = [
  "name",
  "description",
  "when_to_use",
  "effort",
  "estimated_tokens",
  "allowed_tools",
  "capability_grants",
] as const

/**
 * Validate skill kernel projection result at runtime.
 *
 * Throws if:
 * - Any forbidden field is present in the result
 * - Any required preserved field is missing
 * - Any unexpected field is present (strict mode)
 *
 * @throws {Error} If validation fails
 */
export function validateSkillKernelProjection(
  result: unknown,
  options: { strict?: boolean } = {}
): asserts result is KernelSkillMetadata {
  if (typeof result !== "object" || result === null) {
    throw new Error(
      "Skill kernel projection validation failed: result must be an object"
    )
  }

  const obj = result as Record<string, unknown>

  // Check forbidden fields (security policy)
  for (const field of FORBIDDEN_FIELDS) {
    if (field in obj) {
      throw new Error(
        `Skill kernel projection validation failed: forbidden field "${field}" leaked across boundary. ` +
        `This violates the host-to-kernel security policy.`
      )
    }
  }

  // Check required preserved fields
  for (const field of REQUIRED_PRESERVED_FIELDS) {
    if (!(field in obj)) {
      throw new Error(
        `Skill kernel projection validation failed: required field "${field}" is missing. ` +
        `The protocol declares this field must be preserved.`
      )
    }
  }

  // Strict mode: check for unexpected fields
  if (options.strict) {
    const allowedSet = new Set(ALLOWED_TARGET_FIELDS)
    for (const key of Object.keys(obj)) {
      if (!allowedSet.has(key as any)) {
        throw new Error(
          `Skill kernel projection validation failed: unexpected field "${key}" in result. ` +
          `Allowed fields: ${ALLOWED_TARGET_FIELDS.join(", ")}`
        )
      }
    }
  }
}

/**
 * Type guard for KernelSkillMetadata.
 */
export function isKernelSkillMetadata(value: unknown): value is KernelSkillMetadata {
  try {
    validateSkillKernelProjection(value)
    return true
  } catch {
    return false
  }
}
