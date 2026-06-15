import { startWorkflowTool, submitWorkflowNodesTool } from "../src/types/agent.js"

describe("startWorkflowTool (M5 v1: flatten)", () => {
  it("exposes a spec.nodes batch and shares the submit tool's node-item schema (no drift)", () => {
    expect(startWorkflowTool.name).toBe("start_workflow")
    const p = JSON.parse(startWorkflowTool.parameters)
    expect(p.required).toEqual(["spec"])
    const nodes = p.properties.spec.properties.nodes
    expect(nodes.type).toBe("array")

    // The full control-flow vocabulary is available, same as submit_workflow_nodes.
    const items = nodes.items
    expect(Object.keys(items.properties)).toEqual(
      expect.arrayContaining(["task", "role", "loop", "classify", "tournament", "reducer", "tokenBudget", "dependsOn"]),
    )
    expect(items.required).toEqual(["task", "role"])

    // Same node-item schema as submit_workflow_nodes — they must never drift.
    const submitItems = JSON.parse(submitWorkflowNodesTool.parameters).properties.nodes.items
    expect(items).toEqual(submitItems)
  })
})
