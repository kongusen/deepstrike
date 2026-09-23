import {
  capabilityTool,
  capabilitySkill,
  capabilityMarkerToKernel,
  capabilityMountToKernel,
  capabilityUnmountToKernel,
} from "../src/runtime/kernel-step.js"

describe("capability host-to-kernel adapters", () => {
  it("projects tool and skill capabilities without leaking host extensions", () => {
    expect(capabilityTool({
      name: "lookup",
      description: "Look up a record",
      parameters: JSON.stringify({ type: "object", properties: {} }),
      providerOptions: { openai: { strict: true } },
    })).toEqual({
      id: "lookup",
      kind: "tool",
      description: "Look up a record",
      tool_schema: { name: "lookup", description: "Look up a record", parameters: { type: "object", properties: {} } },
    })

    expect(capabilitySkill({
      name: "citations",
      description: "Cite sources",
      effort: 2,
      estimatedTokens: 120,
      allowedTools: ["lookup"],
    })).toEqual({
      id: "citations",
      kind: "skill",
      description: "Cite sources",
      skill: { name: "citations", description: "Cite sources", effort: 2, estimated_tokens: 120, allowed_tools: ["lookup"] },
    })
  })

  it("projects marker and lifecycle commands through typed request adapters", () => {
    expect(capabilityMarkerToKernel({ kind: "mount", id: "workspace", description: "Workspace" })).toEqual({
      kind: "mount",
      id: "workspace",
      description: "Workspace",
    })
    expect(capabilityMountToKernel({ capability: { id: "workspace", kind: "mount" }, mountedBy: "test", mountReason: "fixture" })).toEqual({
      kind: "capability_command",
      command: {
        action: "mount",
        capability: { id: "workspace", kind: "mount" },
        mounted_by: "test",
        mount_reason: "fixture",
      },
    })
    expect(capabilityUnmountToKernel({ capabilityKind: "mount", id: "workspace" })).toEqual({
      kind: "capability_command",
      command: { action: "unmount", kind: "mount", id: "workspace" },
    })
  })
})
