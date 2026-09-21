import { createAgent } from "../src/agent-facade.js"

test("SPC-028-28 accepts a model-first AgentDefinition without provider authority", () => {
  const agent = createAgent({ name: "researcher", model: "openai/gpt-5.4", instructions: "research" })
  expect(agent.definition.model).toBe("openai/gpt-5.4")
  expect(agent.definition).not.toHaveProperty("provider")
})

test("SPC-028-29 fails at execution when runtime binding is unresolved", async () => {
  const agent = createAgent({ name: "researcher", model: "openai/gpt-5.4" })
  await expect(agent.run("research")).rejects.toThrow("no runtime provider binding")
})
