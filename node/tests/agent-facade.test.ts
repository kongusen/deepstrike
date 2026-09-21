import { createAgent } from "../src/agent-facade.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { InMemoryMemoryStore } from "../src/memory/in-memory-store.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { tool } from "../src/tools/index.js"

describe("createAgent", () => {
  it("lowers the Agent capability filter into the root runtime ceiling", async () => {
    const provider = new ReplayProvider([{ role: "assistant", content: "done" }])
    const seen: string[][] = []
    const originalStream = provider.stream.bind(provider)
    provider.stream = (async function* (...args: Parameters<typeof provider.stream>) {
      seen.push(args[1].map(schema => schema.name))
      yield* originalStream(...args)
    }) as typeof provider.stream
    const hidden = tool("hidden", "should not be exposed", { type: "object", properties: {} }, () => "hidden")
    const agent = createAgent({
      name: "filtered",
      provider,
      tools: [hidden],
      capabilityFilter: { allowedIds: ["other-tool"] },
    })

    await agent.run("say done")

    expect(seen[0]).not.toContain("hidden")
  })

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

  it("persists multimodal attachments through the public Agent facade", async () => {
    const sessionLog = new InMemorySessionLog()
    const agent = createAgent({
      name: "vision",
      provider: new ReplayProvider([{ role: "assistant", content: "seen" }]),
      sessionLog,
    })

    await agent.run("describe this", {
      session: { id: "vision-session" },
      attachments: [{ type: "image", source: { kind: "url", url: "https://storage.test/image" }, mediaType: "image/png" }],
    })

    const started = (await sessionLog.read("vision-session")).find(entry => entry.event.kind === "run_started")
    expect(started?.event.kind === "run_started" ? started.event.attachments : undefined).toEqual([
      { type: "image", source: { kind: "url", url: "https://storage.test/image" }, mediaType: "image/png" },
    ])
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
