import type { ToolSchema, ToolResult } from "../types.js"

/** M3/G4: the runtime context a tool may read when executing (carries the working directory). A
 *  narrow, dependency-free shape; the execution plane's `RunContext` is structurally assignable to it.
 *  (WASM has no filesystem, so worktree isolation is N/A here — this keeps the tool ABI in parity
 *  with the Node/Python ports so a tool authored once works across all of them.) */
export interface ToolExecContext {
  cwd?: string
}

export interface RegisteredTool {
  schema: ToolSchema
  execute(args: Record<string, unknown>, ctx?: ToolExecContext): Promise<string>
}

export function tool(
  name: string,
  description: string,
  parameters: Record<string, unknown>,
  fn: (args: Record<string, unknown>, ctx?: ToolExecContext) => Promise<string> | string,
): RegisteredTool {
  return {
    schema: { name, description, parameters: JSON.stringify(parameters) },
    async execute(args, ctx) { return fn(args, ctx) },
  }
}

export async function executeTools(
  calls: { id: string; name: string; arguments: string }[],
  registry: Map<string, RegisteredTool>,
): Promise<ToolResult[]> {
  return Promise.all(calls.map(async c => {
    const t = registry.get(c.name)
    if (!t) return { callId: c.id, output: `unknown tool: ${c.name}`, isError: true }
    try {
      const args = JSON.parse(c.arguments || "{}")
      return { callId: c.id, output: await t.execute(args), isError: false }
    } catch (err) {
      return { callId: c.id, output: String(err), isError: true }
    }
  }))
}
