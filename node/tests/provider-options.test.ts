import { tool } from "../src/tools/index.js"
import type { AgentRunSpec } from "../src/types/agent.js"
import type { ToolSchema } from "../src/types.js"

describe("spc_001-01: providerOptions additive field", () => {
  it("AgentRunSpec accepts providerOptions and round-trips through JSON, preserving unknown vendor keys", () => {
    const spec: AgentRunSpec = {
      identity: { agentId: "a1", sessionId: "s1", isSubAgent: false },
      role: "custom",
      goal: "test",
      providerOptions: { openai: { reasoningEffort: "high" }, anthropic: { betas: ["x"] }, someFutureVendor: { z: 1 } },
    }
    const roundTripped = JSON.parse(JSON.stringify(spec)) as AgentRunSpec
    expect(roundTripped.providerOptions).toEqual(spec.providerOptions)
  })

  it("AgentRunSpec still constructs with providerOptions omitted (zero regression)", () => {
    const spec: AgentRunSpec = {
      identity: { agentId: "a1", sessionId: "s1", isSubAgent: false },
      role: "custom",
      goal: "test",
    }
    expect(spec.providerOptions).toBeUndefined()
  })

  it("RegisteredTool (via tool()) accepts providerOptions and round-trips it", () => {
    const registered = tool("noop", "does nothing", { type: "object", properties: {} }, () => "ok")
    const withOptions = { ...registered, providerOptions: { openai: { strict: true } } }
    const roundTripped = JSON.parse(JSON.stringify(withOptions))
    expect(roundTripped.providerOptions).toEqual({ openai: { strict: true } })
  })

  it("ToolSchema accepts providerOptions and round-trips it, preserving unknown vendor keys", () => {
    const schema: ToolSchema = {
      name: "noop",
      description: "does nothing",
      parameters: JSON.stringify({ type: "object", properties: {} }),
      providerOptions: { anthropic: { cacheControl: { type: "ephemeral" } }, unknownVendor: { keep: true } },
    }
    const roundTripped = JSON.parse(JSON.stringify(schema)) as ToolSchema
    expect(roundTripped.providerOptions).toEqual(schema.providerOptions)
  })
})
