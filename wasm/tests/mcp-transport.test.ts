import { McpExecutionPlane } from "../src/runtime/mcp-transport.js"

test("browser MCP transport is host-injected and lifecycle managed", async () => {
  let started = 0
  let stopped = 0
  const plane = new McpExecutionPlane([{ name: "remote", transport: { kind: "http", url: "https://mcp.test" } }], async () => ({
    async start() { started += 1 },
    schemas: () => [{ name: "remote_tool", description: "remote", parameters: "{}" }],
    async execute() { return { content: "ok", isError: false } },
    async stop() { stopped += 1 },
  }))
  await plane.connect()
  await plane.connect()
  expect(started).toBe(1)
  expect(plane.schemas().map(schema => schema.name)).toEqual(["remote_tool"])
  await plane.disconnect()
  expect(stopped).toBe(1)
})
