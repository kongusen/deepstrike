import type { ToolCall, ToolSchema, StreamEvent } from "../types.js"
import type { ToolResultEvent } from "../types.js"
import type { RegisteredTool } from "../tools/index.js"
import type { ExecutionPlane, RunContext } from "./execution-plane.js"
import { LocalExecutionPlane } from "./execution-plane.js"
import type { CredentialVault } from "./credential-vault.js"
import { formatToolError } from "../tools/errors.js"

export interface RemoteVpcOptions {
  /**
   * Base URL of the remote worker endpoint inside the customer VPC.
   * Expected routes:
   *   POST {baseUrl}/execute  body: { name, arguments }  response: { output, isError }
   *   GET  {baseUrl}/schemas  response: ToolSchema[]    (optional — or provide `schemas` statically)
   */
  baseUrl: string
  vault: CredentialVault
  /**
   * Vault key whose value is sent verbatim as the Authorization header.
   * The credential is fetched once per executeAll call (not cached across calls).
   */
  authCredentialKey?: string
  /**
   * Static tool schemas served by this VPC worker.
   * Provide these when the remote /schemas route is unavailable or schema drift
   * is undesirable at runtime.
   */
  schemas: ToolSchema[]
  /** Per-call HTTP timeout in ms. Defaults to 30 000. */
  timeoutMs?: number
}

/**
 * ExecutionPlane that forwards tool calls over HTTP to a worker inside a customer VPC.
 *
 * Credentials are stored in a CredentialVault and injected into HTTP headers at call
 * time — they are never forwarded to the model or stored in the session log.
 *
 * Local tools (registered via `register()`) run in-process and take priority over
 * any remote schema with the same name.
 */
export class RemoteVpcPlane implements ExecutionPlane {
  private readonly remoteSchemas: ToolSchema[]
  private readonly remoteNames: Set<string>
  private localPlane = new LocalExecutionPlane()

  constructor(private readonly opts: RemoteVpcOptions) {
    this.remoteSchemas = opts.schemas
    this.remoteNames = new Set(opts.schemas.map(s => s.name))
  }

  register(...tools: RegisteredTool[]): this {
    this.localPlane.register(...tools)
    return this
  }

  unregister(name: string): this {
    this.localPlane.unregister(name)
    return this
  }

  schemas(): ToolSchema[] {
    // Local tools shadow remote tools with the same name
    const localNames = new Set(this.localPlane.schemas().map(s => s.name))
    const remoteVisible = this.remoteSchemas.filter(s => !localNames.has(s.name))
    return [...this.localPlane.schemas(), ...remoteVisible]
  }

  async *executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent> {
    const localSchemaNames = new Set(this.localPlane.schemas().map(s => s.name))
    const localCalls  = calls.filter(c => localSchemaNames.has(c.name))
    const remoteCalls = calls.filter(c => !localSchemaNames.has(c.name))

    if (localCalls.length > 0) yield* this.localPlane.executeAll(localCalls, ctx)

    if (remoteCalls.length === 0) return

    const auth = this.opts.authCredentialKey
      ? await this.opts.vault.get(this.opts.authCredentialKey)
      : undefined

    const headers: Record<string, string> = { "Content-Type": "application/json" }
    if (auth) headers["Authorization"] = auth

    // Fire all remote calls concurrently; yield results in dispatch order
    const pending = remoteCalls.map(call => this.callRemote(call, headers).then(result => ({ call, result })))
    for (const p of pending) {
      const { call, result } = await p
      yield {
        type: "tool_result",
        callId: call.id,
        name: call.name,
        content: result.output,
        isError: result.isError,
      } as ToolResultEvent
    }
  }

  private async callRemote(
    call: ToolCall,
    headers: Record<string, string>,
  ): Promise<{ output: string; isError: boolean }> {
    try {
      const args = JSON.parse(call.arguments || "{}") as Record<string, unknown>
      const response = await fetch(`${this.opts.baseUrl}/execute`, {
        method: "POST",
        headers,
        body: JSON.stringify({ name: call.name, arguments: args }),
        signal: AbortSignal.timeout(this.opts.timeoutMs ?? 30_000),
      })
      if (!response.ok) {
        const body = await response.text().catch(() => "")
        return { output: `HTTP ${response.status}${body ? `: ${body}` : ""}`, isError: true }
      }
      const result = await response.json() as { output: string; isError?: boolean }
      return { output: result.output, isError: result.isError ?? false }
    } catch (err) {
      return { output: formatToolError(err), isError: true }
    }
  }
}
