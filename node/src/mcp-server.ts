/** spc_001 §2.3: public MCP server contract. A parallel new type — not a replacement for
 *  `McpServerConfig` (`runtime/mcp-proxy-plane.ts`), which is the stdio-only execution-layer
 *  config `McpProxyPlane` actually spawns. `MCPServer` is the public IR shape; lowering it down
 *  to `McpServerConfig` (or an equivalent per transport kind) is separate, later work. */
export type McpTransport =
  | { kind: "stdio"; command: string; args?: string[] }
  | { kind: "http"; url: string }
  | { kind: "sse"; url: string }
  | { kind: "custom"; [key: string]: unknown }

export interface MCPServer {
  name?: string
  transport: McpTransport
  tools?: string[]
  resources?: boolean
  prompts?: boolean
  auth?: Record<string, unknown>
  metadata?: Record<string, unknown>
}
