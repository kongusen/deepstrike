/**
 * Generated runtime validator for the skill host-to-kernel boundary.
 * DO NOT EDIT BY HAND - regenerate with: npm run contracts:check
 */

import type { KernelSkillMetadata } from "../kernel-step.js"

const FORBIDDEN_FIELDS = [
  "provider_credentials",
  "activation_authority",
  "storage_backend",
  "user_storage_path",
  "source_adapter"
] as const
const REQUIRED_PRESERVED_FIELDS = [
  "name",
  "description"
] as const
const ALLOWED_TARGET_FIELDS = [
  "name",
  "description",
  "when_to_use",
  "effort",
  "estimated_tokens",
  "allowed_tools",
  "capability_grants"
] as const

export function validateSkillKernelProjection(
  result: unknown,
  options: { strict?: boolean } = {},
): asserts result is KernelSkillMetadata {
  if (typeof result !== "object" || result === null) {
    throw new Error("Skill kernel projection validation failed: result must be an object")
  }

  const object = result as Record<string, unknown>
  for (const field of FORBIDDEN_FIELDS) {
    if (field in object) {
      throw new Error(
        `Skill kernel projection validation failed: forbidden field "${field}" leaked across boundary.`,
      )
    }
  }
  for (const field of REQUIRED_PRESERVED_FIELDS) {
    if (!Object.prototype.hasOwnProperty.call(object, field)) {
      throw new Error(
        `Skill kernel projection validation failed: required field "${field}" is missing.`,
      )
    }
  }
  if (options.strict) {
    const allowed = new Set<string>(ALLOWED_TARGET_FIELDS)
    for (const key of Object.keys(object)) {
      if (!allowed.has(key)) {
        throw new Error(
          `Skill kernel projection validation failed: unexpected field "${key}" in result.`,
        )
      }
    }
  }
}

export function isKernelSkillMetadata(value: unknown): value is KernelSkillMetadata {
  try {
    validateSkillKernelProjection(value)
    return true
  } catch {
    return false
  }
}
