import { spawn } from "node:child_process"
import { createInterface } from "node:readline"
import type { ToolCall, ToolSchema, StreamEvent } from "../types.js"
import type { ToolResultEvent } from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { LocalExecutionPlane } from "./execution-plane.js"
import type { CredentialVault } from "./credential-vault.js"
import { formatToolError } from "../tools/errors.js"

export interface McpServerConfig {
  /** Executable to run (e.g. "npx", "python3", "/usr/local/bin/my-mcp-server"). */
  command: string
  /** Command-line arguments. */
  args?: string[]
  /**
   * Keys to look up in the vault and inject as env vars with the same name.
   * The credential value is never exposed to the model — only to the subprocess.
   */
  credentialKeys?: string[]
  /** Additional static env vars forwarded to the subprocess. */
  env?: Record<string, string>
}

// ── Internal JSON-RPC / MCP client ───────────────────────────────────────────

interface RpcRequest { jsonrpc: "2.0"; method: string; params?: unknown; id: number }
interface RpcResponse { jsonrpc: "2.0"; result?: unknown; error?: { code: number; message: string }; id: number }

class McpConnection {
  private child: ReturnType<typeof spawn> | null = null
  private pending = new Map<number, { resolve(r: unknown): void; reject(e: Error): void }>()
  private nextId = 1
  private _schemas: ToolSchema[] = []
  private schemaNames = new Set<string>()

  constructor(
    readonly serverName: string,
    private readonly config: McpServerConfig,
    private readonly vault: CredentialVault,
  ) {}

  async start(): Promise<void> {
    const env: Record<string, string> = { ...(process.env as Record<string, string>), ...(this.config.env ?? {}) }
    for (const key of this.config.credentialKeys ?? []) {
      const val = await this.vault.get(key)
      if (val !== undefined) env[key] = val
    }

    this.child = spawn(this.config.command, this.config.args ?? [], {
      env,
      stdio: ["pipe", "pipe", "inherit"],
    })

    const rl = createInterface({ input: this.child.stdout!, crlfDelay: Infinity })
    rl.on("line", line => {
      if (!line.trim()) return
      try {
        const msg = JSON.parse(line) as RpcResponse
        const cb = this.pending.get(msg.id)
        if (!cb) return
        this.pending.delete(msg.id)
        if (msg.error) cb.reject(new Error(`MCP(${this.serverName}) ${msg.error.code}: ${msg.error.message}`))
        else cb.resolve(msg.result)
      } catch { /* ignore malformed lines */ }
    })

    // MCP handshake
    await this.request("initialize", {
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      clientInfo: { name: "deepstrike", version: "0.1.0" },
    })
    // initialized is a notification (no id, no response expected)
    this.notify("notifications/initialized")

    const listResult = await this.request("tools/list") as {
      tools?: Array<{ name: string; description?: string; inputSchema?: Record<string, unknown> }>
    }
    for (const t of listResult.tools ?? []) {
      const schema: ToolSchema = {
        name: t.name,
        description: t.description ?? t.name,
        parameters: JSON.stringify(t.inputSchema ?? { type: "object", properties: {} }),
      }
      this._schemas.push(schema)
      this.schemaNames.add(t.name)
    }
  }

  private request(method: string, params?: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      if (!this.child?.stdin) { reject(new Error(`MCP server "${this.serverName}" not running`)); return }
      const id = this.nextId++
      this.pending.set(id, { resolve, reject })
      const msg: RpcRequest = { jsonrpc: "2.0", method, id }
      if (params !== undefined) msg.params = params
      this.child.stdin.write(JSON.stringify(msg) + "\n")
    })
  }

  private notify(method: string, params?: unknown): void {
    if (!this.child?.stdin) return
    const msg: Record<string, unknown> = { jsonrpc: "2.0", method }
    if (params !== undefined) msg.params = params
    this.child.stdin.write(JSON.stringify(msg) + "\n")
  }

  schemas(): ToolSchema[] { return this._schemas }
  hasSchema(name: string): boolean { return this.schemaNames.has(name) }

  async execute(call: ToolCall): Promise<{ output: string; isError: boolean }> {
    try {
      const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
      const result = await this.request("tools/call", { name: call.name, arguments: args }) as {
        content?: Array<{ type: string; text?: string }>
        isError?: boolean
      }
      const text = (result.content ?? [])
        .filter(c => c.type === "text")
        .map(c => c.text ?? "")
        .join("\n")
      return { output: text || JSON.stringify(result), isError: result.isError ?? false }
    } catch (err) {
      return { output: formatToolError(err), isError: true }
    }
  }

  async stop(): Promise<void> {
    this.child?.kill()
    this.child = null
    for (const cb of this.pending.values()) cb.reject(new Error(`MCP server "${this.serverName}" stopped`))
    this.pending.clear()
  }
}

// ── Public plane ──────────────────────────────────────────────────────────────

/**
 * ExecutionPlane that proxies tool calls to MCP servers.
 *
 * Credentials live in a CredentialVault and are injected into each server's
 * subprocess environment — the model never sees the credential values.
 *
 * Usage:
 *   const plane = new McpProxyPlane({
 *     servers: { brave: { command: "npx", args: ["-y", "@modelcontextprotocol/server-brave-search"], credentialKeys: ["BRAVE_API_KEY"] } },
 *     vault: new EnvCredentialVault(),
 *   })
 *   await plane.connect()
 *   // ... use with RuntimeRunner ...
 *   await plane.disconnect()
 */
export class McpProxyPlane implements ExecutionPlane {
  private connections = new Map<string, McpConnection>()
  private toolToConn = new Map<string, McpConnection>()
  private localPlane = new LocalExecutionPlane()
  private localNames = new Set<string>()

  constructor(private readonly opts: {
    servers: Record<string, McpServerConfig>
    vault: CredentialVault
  }) {}

  /** Start all configured MCP server processes and discover their tool schemas. */
  async connect(): Promise<void> {
    for (const [name, config] of Object.entries(this.opts.servers)) {
      const conn = new McpConnection(name, config, this.opts.vault)
      await conn.start()
      this.connections.set(name, conn)
      for (const schema of conn.schemas()) this.toolToConn.set(schema.name, conn)
    }
  }

  /** Gracefully stop all MCP server processes. */
  async disconnect(): Promise<void> {
    for (const conn of this.connections.values()) await conn.stop()
    this.connections.clear()
    this.toolToConn.clear()
  }

  register(...tools: RegisteredTool[]): this {
    this.localPlane.register(...tools)
    for (const t of tools) this.localNames.add(t.schema.name)
    return this
  }

  unregister(name: string): this {
    this.localPlane.unregister(name)
    this.localNames.delete(name)
    return this
  }

  schemas(): ToolSchema[] {
    const mcp: ToolSchema[] = []
    for (const conn of this.connections.values()) mcp.push(...conn.schemas())
    return [...this.localPlane.schemas(), ...mcp]
  }

  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    const localCalls = calls.filter(c => this.localNames.has(c.name))
    const mcpCalls   = calls.filter(c => !this.localNames.has(c.name))

    if (localCalls.length > 0) yield* this.localPlane.executeAll(localCalls, ctx)

    // MCP calls are inherently sequential per server (JSON-RPC request/response)
    // but different servers can be called concurrently.
    const groups = new Map<McpConnection, ToolCall[]>()
    const unknown: ToolCall[] = []
    for (const call of mcpCalls) {
      const conn = this.toolToConn.get(call.name)
      if (!conn) { unknown.push(call); continue }
      if (!groups.has(conn)) groups.set(conn, [])
      groups.get(conn)!.push(call)
    }

    for (const call of unknown) {
      yield { type: "tool_result", callId: call.id, name: call.name, content: `unknown MCP tool: ${call.name}`, isError: true } as ToolResultEvent
    }

    const tasks = Array.from(groups.entries()).map(async ([conn, serverCalls]) => {
      const results: Array<{ call: ToolCall; result: { output: string; isError: boolean } }> = []
      for (const call of serverCalls) results.push({ call, result: await conn.execute(call) })
      return results
    })

    for (const settled of await Promise.all(tasks)) {
      for (const { call, result } of settled) {
        yield { type: "tool_result", callId: call.id, name: call.name, content: result.output, isError: result.isError } as ToolResultEvent
      }
    }
  }
}
