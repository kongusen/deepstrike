import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool, streamingTool } from "../../src/tools/index.js"

describe("LocalExecutionPlane", () => {
  it("denies tools when governance returns deny", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("secret", "Secret", { type: "object", properties: {} }, () => "leaked"))

    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "c1", name: "secret", arguments: "{}" }],
      {
        governance: {
          evaluate: () => ({ kind: "deny", reason: "blocked" }),
        },
      },
    )) {
      events.push(evt)
    }

    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "c1",
      isError: true,
    }))
  })

  it("runs regular tools concurrently and emits one result per call", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(
      tool("slow_a", "A", { type: "object", properties: {} }, async () => {
        await new Promise(r => setTimeout(r, 5))
        return "A"
      }),
      tool("slow_b", "B", { type: "object", properties: {} }, async () => "B"),
    )

    const results = []
    for await (const evt of plane.executeAll(
      [
        { id: "a", name: "slow_a", arguments: "{}" },
        { id: "b", name: "slow_b", arguments: "{}" },
      ],
      {},
    )) {
      if (evt.type === "tool_result") results.push(evt)
    }

    expect(results).toHaveLength(2)
    expect(results.map(r => r.content).sort()).toEqual(["A", "B"])
  })

  it("supports suspend/resume via onToolSuspend", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(streamingTool("wait", "Wait", { type: "object", properties: {} }, async function* () {
      const resumed = yield { type: "suspend", suspensionId: "t1" }
      yield { type: "text", text: String(resumed) }
    }))

    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "c1", name: "wait", arguments: "{}" }],
      { onToolSuspend: async () => "ok" },
    )) {
      events.push(evt)
    }

    expect(events).toContainEqual(expect.objectContaining({ type: "tool_suspend", suspensionId: "t1" }))
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "c1",
      content: "ok",
      isError: false,
    }))
  })

  it("returns error for unknown tool without throwing", async () => {
    const plane = new LocalExecutionPlane()
    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "x", name: "missing", arguments: "{}" }],
      {},
    )) {
      events.push(evt)
    }
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "x",
      isError: true,
    }))
  })
})
