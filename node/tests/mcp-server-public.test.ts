import type { MCPServer, McpTransport } from "../src/mcp-server.js"

// Compile-time only: proves the discriminated union narrows correctly per `kind`. Never called;
// a type error here would fail `tsc`, which the regular test run does not exercise, so this
// function exists purely so an editor/CI type-check catches a narrowing regression.
function _typeNarrowingCheck(transport: McpTransport): string {
  if (transport.kind === "stdio") return transport.command
  if (transport.kind === "http") return transport.url
  if (transport.kind === "sse") return transport.url
  return "custom"
}
void _typeNarrowingCheck

describe("spc_001-07: MCPServer public type", () => {
  it("constructs with a stdio transport and exposes its fields", () => {
    const server: MCPServer = {
      name: "filesystem",
      transport: { kind: "stdio", command: "npx", args: ["-y", "@modelcontextprotocol/server-filesystem"] },
      tools: ["read_file", "write_file"],
    }
    expect(server.name).toBe("filesystem")
    expect(server.transport).toEqual({
      kind: "stdio",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-filesystem"],
    })
    expect(server.tools).toEqual(["read_file", "write_file"])
  })

  it.each<[McpTransport["kind"], McpTransport]>([
    ["stdio", { kind: "stdio", command: "python3" }],
    ["http", { kind: "http", url: "https://example.com/mcp" }],
    ["sse", { kind: "sse", url: "https://example.com/sse" }],
    ["custom", { kind: "custom", foo: "bar" }],
  ])("constructs every transport kind (%s)", (kind, transport) => {
    const server: MCPServer = { transport }
    expect(server.transport.kind).toBe(kind)
  })
})
