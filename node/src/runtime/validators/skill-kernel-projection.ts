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
const LAZY_FIELDS = [
  "instructions",
  "resource_contents",
  "scripts",
  "assets"
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
const TARGET_FIELD_SHAPES = {
  "name": "string",
  "description": "string",
  "when_to_use": "string",
  "effort": "number",
  "estimated_tokens": "number",
  "allowed_tools": "array:string",
  "capability_grants": "array:object"
} as const

function matchesShape(value: unknown, shape: string): boolean {
  if (shape === "string") return typeof value === "string"
  if (shape === "number") return typeof value === "number" && Number.isFinite(value)
  if (shape === "boolean") return typeof value === "boolean"
  if (shape === "array:string") return Array.isArray(value) && value.every(item => typeof item === "string")
  if (shape === "array:object") return Array.isArray(value) && value.every(item => typeof item === "object" && item !== null && !Array.isArray(item))
  if (shape === "array") return Array.isArray(value)
  if (shape === "object") return typeof value === "object" && value !== null && !Array.isArray(value)
  return true
}

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
  for (const field of LAZY_FIELDS) {
    if (field in object) {
      throw new Error(
        `Skill kernel projection validation failed: lazy field "${field}" must not be materialized in the kernel metadata projection (progressive disclosure).`,
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
  for (const field of Object.keys(TARGET_FIELD_SHAPES)) {
    if (Object.prototype.hasOwnProperty.call(object, field) && !matchesShape(object[field], TARGET_FIELD_SHAPES[field as keyof typeof TARGET_FIELD_SHAPES])) {
      throw new Error(`Skill kernel projection validation failed: field "${field}" has an invalid type.`)
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
