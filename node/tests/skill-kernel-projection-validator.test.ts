/**
 * Tests for skill kernel projection runtime validator.
 *
 * Verifies that the validator catches violations TypeScript cannot prevent:
 * - Forbidden field leakage (security policy)
 * - Missing required preserved fields (protocol violation)
 * - Unexpected fields in strict mode
 */

import { describe, it } from "node:test"
import assert from "node:assert"
import { validateSkillKernelProjection } from "../src/runtime/validators/skill-kernel-projection.js"

describe("validateSkillKernelProjection", () => {
  it("accepts a valid kernel skill projection", () => {
    const validProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
    }

    // Should not throw
    assert.doesNotThrow(() => {
      validateSkillKernelProjection(validProjection)
    })
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

    assert.doesNotThrow(() => {
      validateSkillKernelProjection(projectionWithOptionals)
    })
  })

  it("rejects forbidden field: provider_credentials", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      provider_credentials: { apiKey: "secret" }, // FORBIDDEN
    }

    assert.throws(
      () => validateSkillKernelProjection(leakedProjection),
      {
        message: /forbidden field "provider_credentials" leaked/i,
      }
    )
  })

  it("rejects forbidden field: activation_authority", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      activation_authority: "kernel", // FORBIDDEN
    }

    assert.throws(
      () => validateSkillKernelProjection(leakedProjection),
      {
        message: /forbidden field "activation_authority" leaked/i,
      }
    )
  })

  it("rejects forbidden field: storage_backend", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      storage_backend: "filesystem", // FORBIDDEN
    }

    assert.throws(
      () => validateSkillKernelProjection(leakedProjection),
      {
        message: /forbidden field "storage_backend" leaked/i,
      }
    )
  })

  it("rejects forbidden field: user_storage_path", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      user_storage_path: "/home/user/.skills", // FORBIDDEN
    }

    assert.throws(
      () => validateSkillKernelProjection(leakedProjection),
      {
        message: /forbidden field "user_storage_path" leaked/i,
      }
    )
  })

  it("rejects forbidden field: source_adapter", () => {
    const leakedProjection = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      source_adapter: "DirectorySkillSource", // FORBIDDEN
    }

    assert.throws(
      () => validateSkillKernelProjection(leakedProjection),
      {
        message: /forbidden field "source_adapter" leaked/i,
      }
    )
  })

  it("rejects missing required field: name", () => {
    const incompleteProjection = {
      description: "A test skill",
      estimated_tokens: 1000,
      // name is MISSING
    }

    assert.throws(
      () => validateSkillKernelProjection(incompleteProjection),
      {
        message: /required field "name" is missing/i,
      }
    )
  })

  it("rejects missing required field: description", () => {
    const incompleteProjection = {
      name: "test-skill",
      estimated_tokens: 1000,
      // description is MISSING
    }

    assert.throws(
      () => validateSkillKernelProjection(incompleteProjection),
      {
        message: /required field "description" is missing/i,
      }
    )
  })

  it("rejects non-object input", () => {
    assert.throws(
      () => validateSkillKernelProjection(null),
      {
        message: /result must be an object/i,
      }
    )

    assert.throws(
      () => validateSkillKernelProjection("not an object"),
      {
        message: /result must be an object/i,
      }
    )

    assert.throws(
      () => validateSkillKernelProjection(42),
      {
        message: /result must be an object/i,
      }
    )
  })

  it("strict mode: rejects unexpected fields", () => {
    const projectionWithUnexpected = {
      name: "test-skill",
      description: "A test skill",
      estimated_tokens: 1000,
      unexpected_field: "should not be here", // NOT in allowed list
    }

    // Non-strict: accepts unexpected fields
    assert.doesNotThrow(() => {
      validateSkillKernelProjection(projectionWithUnexpected)
    })

    // Strict mode: rejects unexpected fields
    assert.throws(
      () => validateSkillKernelProjection(projectionWithUnexpected, { strict: true }),
      {
        message: /unexpected field "unexpected_field"/i,
      }
    )
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
    assert.doesNotThrow(() => {
      validateSkillKernelProjection(completeProjection, { strict: true })
    })
  })
})
