import { startWorkflowTool, submitWorkflowNodesTool } from "../src/runtime/types/agent.js"

describe("startWorkflowTool (M5 canonical flattening, wasm)", () => {
  it("exposes a spec.nodes batch and shares the submit tool's node-item schema (no drift)", () => {
    expect(startWorkflowTool.name).toBe("start_workflow")
    const p = JSON.parse(startWorkflowTool.parameters)
    expect(p.required).toEqual(["spec"])
    const items = p.properties.spec.properties.nodes.items
    expect(Object.keys(items.properties)).toEqual(
      expect.arrayContaining(["task", "role", "dependsOn"]),
    )
    // Only what the canonical WorkflowNode can run is offered to the model.
    for (const field of ["loop", "classify", "tournament", "reducer", "depPolicy", "trust", "tokenBudget"]) {
      expect(Object.keys(items.properties)).not.toContain(field)
    }
    expect(items.required).toEqual(["task", "role"])

    const submitItems = JSON.parse(submitWorkflowNodesTool.parameters).properties.nodes.items
    expect(items).toEqual(submitItems)
  })
})
