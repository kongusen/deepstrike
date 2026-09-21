import { Agent } from "../src/agent.js"
import { lowerAgent, projectAgentRun, projectAgentContext, projectAgentCapabilities, projectAgentGovernance, projectAgentDelegation } from "../src/agent-ir.js"
import { tool } from "../src/tools/index.js"

test("SPC-028-06 serializes semantic state without a second inputs authority", () => {
  const spec = lowerAgent(new Agent({ name: "researcher", instructions: "original" }))
  expect(spec).not.toHaveProperty("inputs")
  expect(JSON.parse(JSON.stringify(spec))).not.toHaveProperty("inputs")
})

test("SPC-028-06 projections read current spec and cannot mutate it", () => {
  const spec = lowerAgent(new Agent({
    name: "researcher", instructions: "original",
    model: { capability: { vision: true } },
    tools: [tool("search", "Search", { type: "object", properties: {} }, () => "found")],
  }))
  spec.instructions = "updated"
  expect(projectAgentContext(spec).instructions).toBe("updated")
  const context = projectAgentContext(spec)
  context.instructions = "corrupt"
  context.knowledge.push({} as never)
  const run = projectAgentRun(spec)
  if (typeof run.model === "object") run.model.capability!.vision = false
  const capabilities = projectAgentCapabilities(spec)
  capabilities.tools[0].name = "corrupt"
  capabilities.effective.length = 0
  const governance = projectAgentGovernance(spec)
  governance.guardrails.push({} as never)
  const delegation = projectAgentDelegation(spec)
  delegation.handoffs.push({} as never)
  expect(projectAgentRun(spec).model).toEqual({ capability: { vision: true } })
  expect(projectAgentContext(spec)).toMatchObject({ instructions: "updated", knowledge: [] })
  expect(projectAgentCapabilities(spec).tools[0].name).toBe("search")
  expect(projectAgentCapabilities(spec).effective).toHaveLength(1)
  expect(projectAgentGovernance(spec)).toEqual({ guardrails: [] })
  expect(projectAgentDelegation(spec)).toEqual({ handoffs: [] })
  spec.tools[0].name = "renamed"
  spec.capabilityFilter = { allowedIds: ["missing"] }
  expect(projectAgentCapabilities(spec).effective).toEqual([])
  expect(spec.capabilities[0].id).toBe("renamed")
})
