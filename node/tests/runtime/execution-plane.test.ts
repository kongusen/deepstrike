import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool, streamingTool } from "../../src/tools/index.js"
import type { OperationContext } from "../../src/runtime/reliability.js"

describe("LocalExecutionPlane", () => {

  it("passes immutable operation identity to tools", async () => {
    const plane = new LocalExecutionPlane()
    const seen: OperationContext[] = []
    plane.register(tool("observe_context", "Observe context", { type: "object", properties: {} }, (_args, ctx) => {
      if (ctx?.operation) seen.push(ctx.operation)
      return "ok"
    }))
    const operation: OperationContext = {
      runId: "run-1",
      sessionId: "session-1",
      agentId: "agent-1",
      signal: new AbortController().signal,
    }

    for await (const _event of plane.executeAll(
      [{ id: "call-1", name: "observe_context", arguments: "{}" }],
      { operation },
    )) { /* drain */ }

    expect(seen).toHaveLength(1)
    expect(seen[0]).toBe(operation)
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

  it("repairs tool arguments based on schema and yields repair event", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(
      tool(
        "test_repair",
        "Test repair",
        {
          type: "object",
          properties: {
            count: { type: "integer" },
            enabled: { type: "boolean" },
            ratio: { type: "number", default: 0.5 },
          },
          required: ["count"],
        },
        args => JSON.stringify(args),
      ),
    )

    const events = []
    for await (const evt of plane.executeAll(
      [{
        id: "c1",
        name: "test_repair",
        arguments: JSON.stringify({ count: "10", enabled: "true", extra_field: "remove_me" }),
      }],
      {},
    )) {
      events.push(evt)
    }

    // 验证投递了自愈事件
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_argument_repaired",
      callId: "c1",
      name: "test_repair",
      originalArguments: JSON.stringify({ count: "10", enabled: "true", extra_field: "remove_me" }),
      repairedArguments: JSON.stringify({ count: 10, enabled: true, ratio: 0.5 }),
    }))

    // 验证执行结果
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "c1",
      content: JSON.stringify({ count: 10, enabled: true, ratio: 0.5 }),
      isError: false,
    }))
  })
})
