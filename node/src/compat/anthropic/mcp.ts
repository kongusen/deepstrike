import type { MCPServer } from "../../mcp-server.js"

/** spc_007 §3: minimal Anthropic/Claude-style MCP server config shape (the per-server object in
 *  a Claude Desktop-style `mcpServers` map — `{ command, args, env }`), not a live SDK type
 *  import — this adapter reads a serialized config (e.g.
 *  `__fixtures__/anthropic-mcp-config.json`). */
export interface AnthropicMcpConfigJson {
  name?: string
  command: string
  args?: string[]
  env?: Record<string, string>
}

/** Anthropic MCP config → DeepStrike `MCPServer` (spc_001-07). Anthropic's config format is
 *  always a subprocess launch (stdio transport) — there is no http/sse/custom variant to map
 *  from. Pure Surface→Surface mapping, produces only `MCPServer`. */
export function fromAnthropicMcpConfig(json: AnthropicMcpConfigJson): MCPServer {
  return {
    name: json.name,
    transport: { kind: "stdio", command: json.command, args: json.args },
    ...(json.env ? { metadata: { env: json.env } } : {}),
  }
}
