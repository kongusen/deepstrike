import { startWorkflowTool, submitWorkflowNodesTool } from "../src/types/agent.js"

describe("startWorkflowTool (M5 canonical flattening)", () => {
  it("exposes a spec.nodes batch and shares the submit tool's node-item schema (no drift)", () => {
    expect(startWorkflowTool.name).toBe("start_workflow")
    const p = JSON.parse(startWorkflowTool.parameters)
    expect(p.required).toEqual(["spec"])
    const nodes = p.properties.spec.properties.nodes
    expect(nodes.type).toBe("array")

    // Only what the canonical WorkflowNode can run is offered to the model; control-flow kinds the
    // DAG cannot express (loop / classify / tournament / reducer) are not advertised.
    const items = nodes.items
    expect(Object.keys(items.properties)).toEqual(
      expect.arrayContaining(["task", "role", "tokenBudget", "dependsOn"]),
    )
    for (const kind of ["loop", "classify", "tournament", "reducer", "depPolicy", "trust"]) {
      expect(Object.keys(items.properties)).not.toContain(kind)
    }
    expect(items.required).toEqual(["task", "role"])

    // Same node-item schema as submit_workflow_nodes — they must never drift.
    const submitItems = JSON.parse(submitWorkflowNodesTool.parameters).properties.nodes.items
    expect(items).toEqual(submitItems)
  })
})
