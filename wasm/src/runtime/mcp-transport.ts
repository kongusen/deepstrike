import type { ToolCall, ToolSchema, StreamEvent, ToolResultEvent } from "../types.js"
import { LocalExecutionPlane, type ExecutionPlane, type RunContext } from "./execution-plane.js"
import type { RegisteredTool } from "../tools/index.js"

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
  private readonly local = new LocalExecutionPlane()
  private readonly localNames = new Set<string>()
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

  register(...tools: RegisteredTool[]): this { this.local.register(...tools); for (const tool of tools) this.localNames.add(tool.schema.name); return this }
  unregister(name: string): this { this.local.unregister(name); this.localNames.delete(name); this.toolMap.delete(name); return this }
  schemas(): ToolSchema[] { return [...this.local.schemas(), ...this.connections.flatMap(connection => connection.schemas())] }

  async *executeAll(calls: ToolCall[], context: RunContext): AsyncIterable<StreamEvent> {
    const localCalls = calls.filter(call => this.localNames.has(call.name))
    if (localCalls.length) yield* this.local.executeAll(localCalls, context)
    for (const call of calls) {
      if (this.localNames.has(call.name)) continue
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
