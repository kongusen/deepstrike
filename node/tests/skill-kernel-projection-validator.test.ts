/**
 * Tests for skill kernel projection runtime validator.
 *
 * Verifies that the validator catches violations TypeScript cannot prevent:
 * - Forbidden field leakage (security policy)
 * - Missing required preserved fields (protocol violation)
 * - Unexpected fields in strict mode
 */

import { describe, expect, it } from "@jest/globals"
import { validateSkillKernelProjection } from "../src/runtime/validators/skill-kernel-projection.js"

describe("validateSkillKernelProjection", () => {
  it("accepts a valid kernel skill projection", () => {
    const validProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
    }

    // Should not throw
    expect(() => validateSkillKernelProjection(validProjection)).not.toThrow()
  })

  it("accepts optional fields", () => {
    const projectionWithOptionals = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      when_to_use: "Use when testing",
      effort: 3,
      allowed_tools: ["read", "write"],
      capability_grants: [{ type: "filesystem" }],
    }

    expect(() => validateSkillKernelProjection(projectionWithOptionals)).not.toThrow()
  })

  it("rejects forbidden field: provider_credentials", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      provider_credentials: { apiKey: "secret" }, // FORBIDDEN
    }

    expect(() => validateSkillKernelProjection(leakedProjection)).toThrow(/forbidden field "provider_credentials" leaked/i)
  })

  it("rejects forbidden field: activation_authority", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      activation_authority: "kernel", // FORBIDDEN
    }

    expect(() => validateSkillKernelProjection(leakedProjection)).toThrow(/forbidden field "activation_authority" leaked/i)
  })

  it("rejects forbidden field: storage_backend", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      storage_backend: "filesystem", // FORBIDDEN
    }

    expect(() => validateSkillKernelProjection(leakedProjection)).toThrow(/forbidden field "storage_backend" leaked/i)
  })

  it("rejects forbidden field: user_storage_path", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      user_storage_path: "/home/user/.skills", // FORBIDDEN
    }

    expect(() => validateSkillKernelProjection(leakedProjection)).toThrow(/forbidden field "user_storage_path" leaked/i)
  })

  it("rejects forbidden field: source_adapter", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      source_adapter: "DirectorySkillSource", // FORBIDDEN
    }

    expect(() => validateSkillKernelProjection(leakedProjection)).toThrow(/forbidden field "source_adapter" leaked/i)
  })

  it("rejects missing required field: name", () => {
    const incompleteProjection = {
      description: "A test skill",
      estimated_tokens: 1000,
      // name is MISSING
    }

    expect(() => validateSkillKernelProjection(incompleteProjection)).toThrow(/required field "name" is missing/i)
  })

  it("rejects missing required field: description", () => {
    const incompleteProjection = {
      name: "test-skill",
      estimated_tokens: 1000,
      // description is MISSING
    }

    expect(() => validateSkillKernelProjection(incompleteProjection)).toThrow(/required field "description" is missing/i)
  })

  it("rejects non-object input", () => {
    expect(() => validateSkillKernelProjection(null)).toThrow(/result must be an object/i)
    expect(() => validateSkillKernelProjection("not an object")).toThrow(/result must be an object/i)
    expect(() => validateSkillKernelProjection(42)).toThrow(/result must be an object/i)
  })

  it("strict mode: rejects unexpected fields", () => {
    const projectionWithUnexpected = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      unexpected_field: "should not be here", // NOT in allowed list
    }

    // Non-strict: accepts unexpected fields
    expect(() => validateSkillKernelProjection(projectionWithUnexpected)).not.toThrow()

    // Strict mode: rejects unexpected fields
    expect(() => validateSkillKernelProjection(projectionWithUnexpected, { strict: true })).toThrow(/unexpected field "unexpected_field"/i)
  })

  it("strict mode: accepts all allowed fields", () => {
    const completeProjection = {
      name: "test-skill",
      description: "A test skill",
      when_to_use: "Use when testing",
      effort: 3,
      estimated_tokens: 1000,
      allowed_tools: ["read", "write"],
      capability_grants: [{ type: "filesystem" }],
    }

    // Should not throw even in strict mode
    expect(() => validateSkillKernelProjection(completeProjection, { strict: true })).not.toThrow()
  })
})
