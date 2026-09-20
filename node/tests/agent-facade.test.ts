import { createAgent } from "../src/agent-facade.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"

describe("createAgent", () => {
  it("runs a goal and returns a structured result", async () => {
    const agent = createAgent({
      name: "researcher",
      provider: new ReplayProvider([{ role: "assistant", content: "done" }]),
    })

    await expect(agent.run("say done")).resolves.toMatchObject({
      output: "done",
      status: "completed",
    })
  })

  it("streams from a reusable agent", async () => {
    const agent = createAgent({
      name: "researcher",
      provider: new ReplayProvider([{ role: "assistant", content: "hello" }]),
    })

    const events: string[] = []
    for await (const event of agent.stream("say hello")) events.push(event.type)

    expect(events).toContain("text_delta")
    expect(events).toContain("done")
  })

  it("exposes memory as an agent capability", async () => {
    const agent = createAgent({
      name: "researcher",
      provider: new ReplayProvider([{ role: "assistant", content: "ok" }]),
      memoryStore: new InMemoryMemoryStore(),
      memoryScope: { tenant_id: "tenant", namespace: "research" },
    })

    const saved = await agent.remember({ name: "project", content: "Use TypeScript" })
    const recalled = await agent.recall("TypeScript")

    expect(saved.content).toBe("Use TypeScript")
    expect(recalled[0]?.record.record_id).toBe(saved.record_id)
  })

  it("delegates a focused task without exposing a runner", async () => {
    const agent = createAgent({
      name: "researcher",
      provider: new ReplayProvider([{ role: "assistant", content: "delegated" }]),
    })

    await expect(agent.delegate({ goal: "inspect the module" })).resolves.toMatchObject({
      output: "delegated",
      status: "completed",
    })
  })
})
