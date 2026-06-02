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

  // ── In-kernel gate mode (governancePolicy) ──────────────────────────────
  // When `kernelGatedCalls` is present, the kernel already enforced deny/rate/param;
  // executeAll must NOT consult the legacy governance.evaluate and only run human
  // approval for the flagged AskUser calls.

  it("kernel-gate mode executes ungated calls without consulting SDK evaluate", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("safe", "Safe", { type: "object", properties: {} }, () => "ran"))
    let evaluated = false

    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "c1", name: "safe", arguments: "{}" }],
      {
        kernelGatedCalls: new Map(),
        // Should be ignored entirely in kernel-gate mode.
        governance: { evaluate: () => { evaluated = true; return { kind: "deny" } } },
      },
    )) {
      events.push(evt)
    }

    expect(evaluated).toBe(false)
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result", callId: "c1", content: "ran", isError: false,
    }))
  })

  it("kernel-gate mode runs human approval for gated calls and executes on approval", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("danger", "Danger", { type: "object", properties: {} }, () => "done"))

    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "c1", name: "danger", arguments: "{}" }],
      {
        kernelGatedCalls: new Map([["c1", "needs approval"]]),
        onPermissionRequest: () => true,
      },
    )) {
      events.push(evt)
    }

    expect(events).toContainEqual(expect.objectContaining({ type: "permission_request", callId: "c1" }))
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result", callId: "c1", content: "done", isError: false,
    }))
  })

  it("kernel-gate mode denies gated call when no approval handler is configured", async () => {
    const plane = new LocalExecutionPlane()
    plane.register(tool("danger", "Danger", { type: "object", properties: {} }, () => "done"))

    const events = []
    for await (const evt of plane.executeAll(
      [{ id: "c1", name: "danger", arguments: "{}" }],
      { kernelGatedCalls: new Map([["c1", "needs approval"]]) },
    )) {
      events.push(evt)
    }

    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result", callId: "c1", isError: true, errorKind: "governance_denied",
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
