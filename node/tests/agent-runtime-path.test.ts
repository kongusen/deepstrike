import { fileURLToPath } from "node:url"
import { createAgent } from "../src/index.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import { tool } from "../src/tools/index.js"

class CapturingProvider extends ReplayProvider {
  readonly requests: Parameters<ReplayProvider["stream"]>[] = []

  override async *stream(...args: Parameters<ReplayProvider["stream"]>) {
    this.requests.push(args)
    yield* super.stream(...args)
  }
}

describe("public Agent runtime path", () => {
  it("advertises and executes a declared tool across provider turns", async () => {
    let calls = 0
    const lookup = tool("lookup", "Look up a fact", { type: "object", properties: {} }, () => {
      calls++
      return "found"
    })
    const provider = new CapturingProvider([
      { role: "assistant", content: "", toolCalls: [{ id: "lookup-1", name: "lookup", arguments: "{}" }] },
      { role: "assistant", content: "found" },
    ])
    const agent = createAgent({ tools: [lookup], runtimeBinding: { provider } })

    const result = await agent.run("look up a fact")

    expect(provider.requests.map(([, schemas]) => schemas.map(schema => schema.name))).toEqual([["lookup"], ["lookup"]])
    expect(calls).toBe(1)
    expect(result).toMatchObject({ status: "completed", output: "found" })
  })

  it("advertises a custom execution plane's tools under the Agent ceiling", async () => {
    const plane = new LocalExecutionPlane().register(
      tool("visible", "Allowed by the Agent", { type: "object", properties: {} }, () => "ok"),
      tool("hidden", "Outside the ceiling", { type: "object", properties: {} }, () => "no"),
    )
    const provider = new CapturingProvider([{ role: "assistant", content: "done" }])
    const agent = createAgent({ runtimeBinding: { provider, executionPlane: plane }, capabilityFilter: { allowedIds: ["visible"] } })

    await agent.run("go")

    expect(provider.requests.map(([, schemas]) => schemas.map(schema => schema.name))).toEqual([["visible"]])
  })

  it.each(["run", "stream", "session", "workflow"] as const)("forwards provider namespaces through %s", async mode => {
    const providerOptions = { openai: { temperature: 0 }, futureVendor: { setting: "keep" } }
    const provider = new CapturingProvider([{ role: "assistant", content: "done" }])
    const agent = createAgent({ providerOptions, runtimeBinding: { provider } })

    if (mode === "run") await agent.run("go")
    else if (mode === "stream") { for await (const _event of agent.stream("go")) {} }
    else if (mode === "session") await agent.session("options-session").run("go")
    else await agent.workflow({ nodes: [{ task: { goal: "go" }, role: "explore", contextInheritance: "system_only" }] })

    expect(provider.requests.map(([, , extensions]) => extensions)).toEqual([providerOptions])
  })

  it("preserves extensions when a session resumes a pending provider effect", async () => {
    const providerOptions = { openai: { temperature: 0 } }
    const provider = new CapturingProvider([
      { role: "assistant", content: "interrupted stream" },
      { role: "assistant", content: "resumed" },
    ])
    const session = createAgent({ providerOptions, runtimeBinding: { provider } }).session("resume-options")
    // Stop consuming before the provider result is committed; resume must use the same journal.
    for await (const event of session.stream("go")) {
      if (event.type === "text_delta") break
    }
    const resumed = []
    for await (const event of session.resume()) resumed.push(event)

    expect(resumed).toContainEqual({ type: "text_delta", delta: "resumed" })
    expect(provider.requests.map(([, , extensions]) => extensions)).toEqual([providerOptions, providerOptions])
  })

  it("does not execute a provider call outside the Agent capability ceiling", async () => {
    let hiddenCalls = 0
    const provider = new CapturingProvider([
      { role: "assistant", content: "", toolCalls: [{ id: "hidden-1", name: "hidden", arguments: "{}" }] },
      { role: "assistant", content: "done" },
    ])
    const agent = createAgent({
      tools: [
        tool("visible", "Allowed", { type: "object", properties: {} }, () => "ok"),
        tool("hidden", "Outside the ceiling", { type: "object", properties: {} }, () => { hiddenCalls++; return "no" }),
      ],
      capabilityFilter: { allowedIds: ["visible"] },
      runtimeBinding: { provider },
    })

    await expect(agent.run("go")).resolves.toMatchObject({ status: "completed" })

    expect(provider.requests[0][1].map(schema => schema.name)).toEqual(["visible"])
    expect(hiddenCalls).toBe(0)
  })

  it("keeps both host and Agent vetoes when a runtime policy is supplied", async () => {
    const provider = new CapturingProvider([{ role: "assistant", content: "done" }])
    const agent = createAgent({
      tools: ["host_blocked", "agent_blocked", "allowed"].map(name => tool(name, name, { type: "object", properties: {} }, () => name)),
      guardrails: [{ name: "deny-agent-blocked", policy: { vetoes: ["agent_blocked"] } }],
      runtimeBinding: { provider, runtimeOptions: { governancePolicy: { vetoes: ["host_blocked"] } } },
    })

    await agent.run("go")

    expect(provider.requests.map(([, schemas]) => schemas.map(schema => schema.name))).toEqual([["allowed"]])
  })

  it("preserves host approval requirements while applying Agent guardrails", async () => {
    let calls = 0
    const approvals: string[] = []
    const provider = new CapturingProvider([
      { role: "assistant", content: "", toolCalls: [{ id: "write-1", name: "write", arguments: "{}" }] },
      { role: "assistant", content: "done" },
    ])
    const agent = createAgent({
      tools: ["write", "blocked"].map(name => tool(name, name, { type: "object", properties: {} }, () => { calls++; return "ok" })),
      guardrails: [{ name: "deny-blocked", policy: { vetoes: ["blocked"] } }],
      runtimeBinding: { provider, runtimeOptions: { governancePolicy: { defaultAction: "ask_user" } } },
    })

    await agent.run("write", { onPermissionRequest: event => { approvals.push(event.toolName); return false } })

    expect(provider.requests[0][1].map(schema => schema.name)).toEqual(["write"])
    expect(approvals).toEqual(["write"])
    expect(calls).toBe(0)
  })

  it("discovers MCP tools before setting the baseline and reuses the connection", async () => {
    const provider = new CapturingProvider([
      { role: "assistant", content: "", toolCalls: [{ id: "mcp-1", name: "mcp_echo", arguments: "{}" }] },
      { role: "assistant", content: "done" },
    ], { wrap: true })
    const agent = createAgent({
      runtimeBinding: { provider },
      tools: [tool("local", "Local tool", { type: "object", properties: {} }, () => "local")],
      mcpServers: [{ transport: {
        kind: "stdio",
        command: process.execPath,
        args: [fileURLToPath(new URL("./fixtures/mcp-agent-tool-server.mjs", import.meta.url))],
      } }],
    })

    try {
      for (let run = 0; run < 2; run++) {
        const events = []
        for await (const event of agent.stream("call mcp_echo")) events.push(event)
        expect(events).toContainEqual(expect.objectContaining({ type: "tool_result", name: "mcp_echo", content: "mcp-result", isError: false }))
      }
      expect(provider.requests.map(([, schemas]) => schemas.map(schema => schema.name).sort())).toEqual(
        Array.from({ length: 4 }, () => ["local", "mcp_echo"]),
      )
    } finally {
      await agent.close()
    }
  })
})
