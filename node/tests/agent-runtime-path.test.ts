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
