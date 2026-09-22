import { createInterface } from "node:readline"

const input = createInterface({ input: process.stdin })
input.on("line", line => {
  const request = JSON.parse(line)
  if (request.id === undefined) return
  let result
  switch (request.method) {
    case "initialize":
      result = { protocolVersion: "2024-11-05", capabilities: { tools: {} }, serverInfo: { name: "agent-test", version: "1" } }
      break
    case "tools/list":
      result = { tools: [{ name: "mcp_echo", description: "Echo for Agent tests", inputSchema: { type: "object", properties: {} } }] }
      break
    case "tools/call":
      result = { content: [{ type: "text", text: "mcp-result" }] }
      break
    default:
      throw new Error(`Unexpected method: ${request.method}`)
  }
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\n")
})
