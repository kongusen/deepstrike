/** Capability host-to-kernel projection protocol. */

import type { BoundaryProtocol } from "./types.js"

export const CAPABILITY_HOST_TO_KERNEL_PROTOCOL: BoundaryProtocol = {
  id: "capability.host-to-kernel",
  family: "host-to-kernel",
  direction: "project",
  fields: {
    forbidden: [],
  },
  lazy: {
    lazySemantics: "none",
  },
  adapters: [
    {
      adapter: "runtime/kernel-step:capabilityTool",
      source: { type: "ToolSchema", layer: "host", authority: "host-runtime" },
      target: { type: "KernelCapabilityTool", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["description"],
        drops: ["name", "parameters", "providerOptions"],
        derived: ["id", "kind", "tool_schema"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:capabilitySkill",
      source: { type: "SkillMetadata", layer: "host", authority: "host-runtime" },
      target: { type: "KernelCapabilitySkill", layer: "kernel", authority: "kernel" },
      fields: {
        preserves: ["description"],
        drops: ["name", "whenToUse", "effort", "estimatedTokens", "capabilityGrants", "allowedTools"],
        derived: ["id", "kind", "skill"],
        forbidden: [],
      },
    },
    {
      adapter: "runtime/kernel-step:capabilityMarkerToKernel",
      source: { type: "CapabilityMarkerRequest", layer: "host", authority: "host-runtime" },
      target: { type: "KernelCapabilityMarker", layer: "kernel", authority: "kernel" },
      fields: { preserves: ["kind", "id", "description"], forbidden: [] },
    },
    {
      adapter: "runtime/kernel-step:capabilityMountToKernel",
      source: { type: "CapabilityMountRequest", layer: "host", authority: "host-runtime" },
      target: { type: "KernelCapabilityMountCommand", layer: "kernel", authority: "kernel" },
      fields: { drops: ["capability", "mountedBy", "mountReason"], derived: ["kind", "command"], forbidden: [] },
    },
    {
      adapter: "runtime/kernel-step:capabilityUnmountToKernel",
      source: { type: "CapabilityUnmountRequest", layer: "host", authority: "host-runtime" },
      target: { type: "KernelCapabilityUnmountCommand", layer: "kernel", authority: "kernel" },
      fields: { drops: ["capabilityKind", "id"], derived: ["kind", "command"], forbidden: [] },
    },
  ],
  lossiness: "intentional",
  validation: { mode: "behavioral-tests", reason: "Capability boundary tests cover tool, skill, marker, mount, and unmount projections.", testRefs: ["node/tests/capability-boundary.test.ts", "node/tests/skill-kernel-projection-validator.test.ts"] },
}
