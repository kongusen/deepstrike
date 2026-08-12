import { readFileSync } from "node:fs"
import { join } from "node:path"
import { Agent, lowerAgent, normalizeAgent, type AgentDefinition } from "../src/index.js"

function fixture(): AgentDefinition {
  return JSON.parse(readFileSync(
    join(process.cwd(), "../tests/fixtures/agent-ir/v1-agent.json"),
    "utf8",
  )) as AgentDefinition
}

describe("spc_015-09: Canonical Agent IR", () => {
  it("normalizes the shared fixture without dropping portable, DeepStrike, or unknown namespaced fields", () => {
    const raw = fixture()
    const spec = lowerAgent(normalizeAgent(raw))

    expect(spec.version).toBe(1)
    expect(spec.name).toBe("researcher")
    expect(spec.description).toBe("Finds and verifies source material.")
    expect(spec.instructions).toBe("Cite primary sources and state uncertainty.")
    expect(spec.model).toEqual(raw.model)
    expect(spec.outputSchema).toEqual(raw.outputSchema)
    expect(spec.tools).toEqual(raw.tools)
    expect(spec.memory).toEqual({ kind: "durable", namespace: "project-research" })
    expect(spec.mcpServers).toEqual(raw.mcpServers)
    expect(spec.skills).toEqual(raw.skills)
    expect(spec.knowledge).toEqual(raw.knowledge)
    expect(spec.handoffs).toEqual(raw.handoffs)
    expect(spec.guardrails).toEqual(raw.guardrails)
    expect(spec.metadata).toEqual(raw.metadata)
    expect(spec.extensions).toEqual(raw.providerOptions)
    expect(spec.providerOptions).toEqual(spec.extensions)
    expect(spec.inputs.context.knowledge).toEqual(spec.knowledge)
    expect(spec.inputs.capabilities.tools).toEqual(spec.tools)
    expect(spec.inputs.capabilities.mcpServers).toEqual(spec.mcpServers)
    expect(spec.inputs.capabilities.skills).toEqual(spec.skills)
    expect(spec.inputs.memory).toEqual(spec.memory)
    expect(spec.inputs.delegation.handoffs).toEqual(spec.handoffs)
    expect(spec.inputs.governance.guardrails).toEqual(spec.guardrails)
    expect(spec.capabilityFilter).toEqual(raw.capabilityFilter)
    expect(spec.effectiveCapabilities).toEqual([
      { kind: "tool", id: "web_search", description: "Search the web for source material." },
      { kind: "skill", id: "citations", description: "Citation policy." },
    ])
    expect(spec.inputs.capabilities.effective).toEqual(spec.effectiveCapabilities)
  })

  it("returns an isolated descriptor and cannot grant undeclared capabilities", () => {
    const agent = normalizeAgent(fixture())
    const spec = lowerAgent(agent)
    agent.providerOptions!.openai = { reasoningEffort: "low" }
    agent.metadata!.team = "changed"

    expect(spec.extensions.openai).toEqual({ reasoningEffort: "high" })
    expect(spec.metadata).toEqual({ team: "research", priority: 1 })

    const filtered = lowerAgent(new Agent({
      name: "declared-only",
      tools: [{ name: "read", parameters: { type: "object", properties: {} } }],
      capabilityFilter: { allowedKinds: ["tool"], allowedIds: ["not-declared"] },
    }))
    expect(filtered.capabilities).toEqual([{ kind: "tool", id: "read", description: "" }])
    expect(filtered.effectiveCapabilities).toEqual([])
  })
})
