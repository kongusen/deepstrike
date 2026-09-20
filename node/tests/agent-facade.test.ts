import { createAgent } from "../src/agent-facade.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"

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
})
