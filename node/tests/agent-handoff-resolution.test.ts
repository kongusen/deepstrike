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
    const targetProvider = new ReplayProvider([{ role: "assistant", content: "reviewed by target" }])
    const target = createAgent({
      name: "reviewer",
      runtimeBinding: { provider: targetProvider },
    })
    const source = createAgent({
      name: "writer",
      handoffs: [{
        agent: "reviewer",
        inputSchema: { type: "object", required: ["draft"] },
        metadata: { source: "declared" },
        providerOptions: { anthropic: { effort: "high" } },
      }],
      runtimeBinding: {
        provider: new ReplayProvider([{ role: "assistant", content: "wrong provider" }]),
        resolveAgent: name => name === "reviewer" ? target : undefined,
      },
    })

    await expect(source.delegate({
      goal: "review",
      target: "reviewer",
      input: { draft: "text" },
      metadata: { requestId: "r1" },
      providerOptions: { anthropic: { cache: true } },
    })).resolves.toMatchObject({
      output: "reviewed by target",
      status: "completed",
    })
    expect(targetProvider.consumed()).toBe(1)
    await expect(source.delegate({ goal: "review", target: "reviewer", input: {} })).rejects.toThrow(/schema validation/i)
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
