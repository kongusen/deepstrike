import { createWorkflow, lowerWorkflowDefinition } from "../src/workflow/definition.js"

test("SPC-028-39/41 public workflow uses named Agent steps and lowers to runtime nodes", () => {
  const definition = createWorkflow({
    steps: {
      research: { agent: { name: "researcher" }, input: "Research", dependsOn: [] },
      write: { agent: "writer", input: "Write", dependsOn: ["research"] },
    },
  })
  const lowered = lowerWorkflowDefinition(definition)
  expect(lowered.nodes).toHaveLength(2)
  expect(lowered.nodes[1].dependsOn).toEqual([0])
  expect(lowered.nodes[0].agent).toBe("researcher")
})

test("SPC-028-39 lowering rejects dependencies outside the workflow", () => {
  expect(() => lowerWorkflowDefinition({
    steps: { write: { agent: "writer", input: "Write", dependsOn: ["missing"] } },
  })).toThrow('workflow step "write" depends on unknown step "missing"')
})
