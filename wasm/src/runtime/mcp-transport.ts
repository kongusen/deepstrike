import type { ToolCall, ToolSchema, StreamEvent, ToolResultEvent } from "../types.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"

/** Browser-safe MCP connection boundary. Implement HTTP/SSE/custom transports in the host. */
export interface McpConnection {
  start(): Promise<void>
  schemas(): ToolSchema[]
  execute(call: ToolCall, context?: RunContext): Promise<{ content: string; isError: boolean; contentParts?: unknown[] }>
  stop(): Promise<void>
}
export interface McpCredentialVault { get(key: string): Promise<string | undefined> }

export interface McpServerConfig {
  name: string
  transport: { kind: "http" | "sse" | "custom"; [key: string]: unknown }
  credentialVault?: McpCredentialVault
}

export type McpConnectionFactory = (config: McpServerConfig) => McpConnection | Promise<McpConnection>

/** ExecutionPlane adapter that keeps MCP wire details outside the Kernel. */
export class McpExecutionPlane implements ExecutionPlane {
  private readonly connections: McpConnection[] = []
  private readonly toolMap = new Map<string, McpConnection>()
  private connected = false
  constructor(private readonly configs: McpServerConfig[], private readonly factory: McpConnectionFactory) {}

  async connect(): Promise<void> {
    if (this.connected) return
    for (const config of this.configs) {
      const connection = await this.factory(config)
      await connection.start()
      this.connections.push(connection)
      for (const schema of connection.schemas()) this.toolMap.set(schema.name, connection)
    }
    this.connected = true
  }

  register(): this { return this }
  unregister(name: string): this { this.toolMap.delete(name); return this }
  schemas(): ToolSchema[] { return this.connections.flatMap(connection => connection.schemas()) }

  async *executeAll(calls: ToolCall[], context: RunContext): AsyncIterable<StreamEvent> {
    for (const call of calls) {
      const connection = this.toolMap.get(call.name)
      if (!connection) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: `unknown MCP tool: ${call.name}`, isError: true } as ToolResultEvent
        continue
      }
      try {
        const result = await connection.execute(call, context)
        yield { type: "tool_result", callId: call.id, name: call.name, content: result.content, isError: result.isError, contentParts: result.contentParts as ToolResultEvent["contentParts"] } as ToolResultEvent
      } catch (error) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: String(error), isError: true } as ToolResultEvent
      }
    }
  }

  async disconnect(): Promise<void> {
    await Promise.all(this.connections.map(connection => connection.stop()))
    this.connections.length = 0
    this.toolMap.clear()
    this.connected = false
  }
}
