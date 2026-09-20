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

  it("turns a claimed signal into an agent run", async () => {
    let acknowledged = false
    let claimed = false
    const signalSource = {
      async claimSignal() {
        if (claimed) return null
        claimed = true
        return {
          deliveryId: "delivery-1",
          leaseToken: "lease-1",
          signalId: "signal-1",
          deliveryAttempt: 1,
          leaseExpiresAtMs: Date.now() + 1000,
          signal: { source: "gateway" as const, signalType: "event" as const, urgency: "normal" as const, payload: { goal: "handle alert" } },
        }
      },
      async ackSignal() { acknowledged = true; return true },
      async nackSignal() { return true },
    }
    const agent = createAgent({
      name: "operator",
      provider: new ReplayProvider([{ role: "assistant", content: "handled" }]),
      runtimeOptions: { signalSource },
    })

    const signalResult = await agent.listen()
    expect(signalResult).toMatchObject({ output: "handled", status: "completed" })
    expect(acknowledged).toBe(true)
  })
})
