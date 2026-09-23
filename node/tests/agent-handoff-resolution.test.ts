import { createAgent } from "../src/index.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"

describe("Agent handoff resolution boundary", () => {
  it("resolves workflow node targets through the same host registry", async () => {
    const target = createAgent({
      name: "reviewer",
      runtimeBinding: { provider: new ReplayProvider([{ role: "assistant", content: "workflow target output" }]) },
    })
    const source = createAgent({
      name: "writer",
      runtimeBinding: {
        provider: new ReplayProvider([{ role: "assistant", content: "wrong workflow provider" }]),
        resolveAgent: name => name === "reviewer" ? target : undefined,
      },
    })

    await expect(source.workflow({
      nodes: [{ task: "review this", role: "verify", agent: "reviewer" }],
    })).resolves.toMatchObject({
      outputs: { "wf-node0": "workflow target output" },
    })
  })

  it("resolves the declared target at the host boundary and runs its provider", async () => {
    const target = createAgent({
      name: "reviewer",
      runtimeBinding: { provider: new ReplayProvider([{ role: "assistant", content: "reviewed by target" }]) },
    })
    const source = createAgent({
      name: "writer",
      handoffs: [{ agent: "reviewer" }],
      runtimeBinding: {
        provider: new ReplayProvider([{ role: "assistant", content: "wrong provider" }]),
        resolveAgent: name => name === "reviewer" ? target : undefined,
      },
    })

    await expect(source.delegate({ goal: "review", target: "reviewer" })).resolves.toMatchObject({
      output: "reviewed by target",
      status: "completed",
    })
  })

  it("fails explicitly when the host resolver cannot find an allowlisted target", async () => {
    const source = createAgent({
      name: "writer",
      handoffs: [{ agent: "reviewer" }],
      runtimeBinding: {
        provider: new ReplayProvider([{ role: "assistant", content: "wrong provider" }]),
        resolveAgent: () => undefined,
      },
    })

    await expect(source.delegate({ goal: "review", target: "reviewer" })).rejects.toThrow(/target agent "reviewer" is not registered/i)
  })
})
