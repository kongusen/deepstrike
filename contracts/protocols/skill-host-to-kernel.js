/**
 * Skill host-to-kernel projection protocol.
 *
 * This crossing projects skill metadata from the host runtime into the kernel's
 * execution vocabulary. The kernel needs skill identity, capabilities, and cost hints,
 * but not storage details, full content, or activation authority.
 *
 * Anthropic skill protocol compatibility:
 * - Skill content (instructions, resources) loads lazily
 * - Kernel receives metadata projection only
 * - Full materialization happens on-demand when kernel activates the skill
 */
/**
 * Skill host-to-kernel projection protocol.
 */
export const SKILL_HOST_TO_KERNEL_PROTOCOL = {
    family: "host-to-kernel",
    direction: "project",
    source: {
        type: "SkillMetadata",
        layer: "host",
        authority: "host-runtime",
    },
    target: {
        type: "KernelSkillMetadata",
        layer: "kernel",
        authority: "kernel",
    },
    fields: {
        // Exact preserves (same name in both types):
        // - name, description are skill identity
        // These will be verified by type inspector in Task 3
        preserves: ["name", "description"],
        // Renames (camelCase → snake_case for kernel wire):
        renames: {
            whenToUse: "when_to_use",
            estimatedTokens: "estimated_tokens",
            allowedTools: "allowed_tools",
            capabilityGrants: "capability_grants",
        },
        // Optional fields (may be undefined in source, omitted in target):
        // whenToUse, effort, estimatedTokens, allowedTools, capabilityGrants, version, digest
        // Intentionally dropped fields:
        drops: [
        // Storage implementation detail (where the SKILL.md lives)
        // Not needed by kernel, which only sees projected metadata
        ],
        // Security policy: fields that MUST NOT cross to kernel
        forbidden: [
            "provider_credentials", // Never expose vendor credentials to kernel
            "activation_authority", // Kernel owns activation state, not metadata
            "storage_backend", // Implementation detail of host loader
            "user_storage_path", // File system paths are host-only
            "source_adapter", // Loader implementation detail
        ],
    },
    lazy: {
        // Skill content fields that are NOT projected to kernel metadata
        lazyFields: [
            "instructions", // Full skill content (loaded on activation)
            "resource_contents", // SKILL.md resources/ directory
            "scripts", // Executable scripts
            "assets", // Binary assets
        ],
        // This crossing preserves lazy semantics:
        // Kernel receives metadata projection, full content loads on activation
        lazySemantics: "preserve",
    },
    adapter: "runtime/kernel-step:skillMetadataToKernel",
    lossiness: "intentional",
};
/**
 * Type guard to validate that a value conforms to the forbidden policy.
 * Generated validator will be created in Task 4.
 */
export function validateSkillKernelCrossing(result) {
    if (typeof result !== "object" || result === null) {
        throw new Error("Skill kernel projection must be an object");
    }
    const obj = result;
    // Check forbidden fields
    for (const field of SKILL_HOST_TO_KERNEL_PROTOCOL.fields.forbidden) {
        if (field in obj) {
            throw new Error(`Forbidden field leaked across skill kernel boundary: ${field}`);
        }
    }
    // Check required preserved fields
    for (const field of SKILL_HOST_TO_KERNEL_PROTOCOL.fields.preserves ?? []) {
        if (!(field in obj)) {
            throw new Error(`Required preserved field missing from skill kernel projection: ${field}`);
        }
    }
}
