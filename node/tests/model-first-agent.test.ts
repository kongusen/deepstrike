import { createAgent } from "../src/agent-facade.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"

test("SPC-028-28 accepts a model-first AgentDefinition without provider authority", () => {
  const agent = createAgent({ name: "researcher", model: "openai/gpt-5.4", instructions: "research" })
  expect(agent.definition.model).toBe("openai/gpt-5.4")
  expect(agent.definition).not.toHaveProperty("provider")
})

test("SPC-028-29 fails at execution when runtime binding is unresolved", async () => {
  const agent = createAgent({ name: "researcher", model: "openai/gpt-5.4" })
  await expect(agent.run("research")).rejects.toThrow("no runtime provider binding")
})

test("SPC-028-29 resolves a model-first binding before constructing the runtime", async () => {
  const provider = new ReplayProvider([{ role: "assistant", content: "routed" }], {
    descriptor: {
      provider: "openai",
      protocol: "openai-chat",
      model: "gpt-5.4",
      reasoning: { supported: true, preserveAcrossToolTurns: true },
      toolCalls: { supported: true, requiresStrictPairing: false },
    },
  })
  const agent = createAgent({
    name: "researcher",
    model: "gpt-5.4",
    runtimeBinding: { providerFor: model => model === "gpt-5.4" ? provider : undefined },
  })

  await expect(agent.run("research")).resolves.toMatchObject({ output: "routed", status: "completed" })
})
