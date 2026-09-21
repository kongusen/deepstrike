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
  expect(lowered.nodes[1].dependsOn).toEqual(["research"])
  expect((lowered.nodes[0] as unknown as { agent: unknown }).agent).toBe("researcher")
})
