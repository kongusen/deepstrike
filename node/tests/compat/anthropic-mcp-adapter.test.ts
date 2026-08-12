import { readFile } from "node:fs/promises"
import { join } from "node:path"
import { fromAnthropicMcpConfig, type AnthropicMcpConfigJson } from "../../src/compat/anthropic/mcp.js"

describe("spc_007-03 ②: fromAnthropicMcpConfig adapter", () => {
  it("maps an Anthropic MCP server config fixture onto an MCPServer with a stdio transport", async () => {
    const raw = await readFile(join(process.cwd(), "src", "__fixtures__", "anthropic-mcp-config.json"), "utf8")
    const fixture = JSON.parse(raw) as AnthropicMcpConfigJson

    const server = fromAnthropicMcpConfig(fixture)

    expect(server.name).toBe(fixture.name)
    expect(server.transport).toEqual({
      kind: "stdio",
      command: fixture.command,
      args: fixture.args,
    })
  })
})
