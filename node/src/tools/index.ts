import type { ToolSchema, ToolResult } from "../types.js"

export interface RegisteredTool {
  schema: ToolSchema
  execute(args: Record<string, unknown>): Promise<string>
}

export function tool(
  name: string,
  description: string,
  parameters: Record<string, unknown>,
  fn: (args: Record<string, unknown>) => Promise<string> | string,
): RegisteredTool {
  return {
    schema: { name, description, parameters: JSON.stringify(parameters) },
    async execute(args) { return fn(args) },
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
      const output = await t.execute(args)
      return { callId: c.id, output, isError: false }
    } catch (err) {
      return { callId: c.id, output: String(err), isError: true }
    }
  }))
}

export const readFile = tool(
  "read_file",
  "Read the contents of a file.",
  { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
  async ({ path }) => {
    const { readFile: fsRead } = await import("fs/promises")
    return fsRead(String(path), "utf8")
  },
)
