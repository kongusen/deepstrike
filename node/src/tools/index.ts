import type { ToolChunk, ToolSchema, ToolResult } from "../types.js"

export interface RegisteredTool {
  schema: ToolSchema
  execute(args: Record<string, unknown>): Promise<string> | AsyncIterable<ToolChunk>
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

export function streamingTool(
  name: string,
  description: string,
  parameters: Record<string, unknown>,
  fn: (args: Record<string, unknown>) => AsyncIterable<ToolChunk>,
): RegisteredTool {
  return {
    schema: { name, description, parameters: JSON.stringify(parameters) },
    execute(args) { return fn(args) },
  }
}

export function isAsyncIterable<T>(value: unknown): value is AsyncIterable<T> {
  return typeof value === "object" && value !== null && Symbol.asyncIterator in value
}

export function normalizeToolChunk(chunk: ToolChunk): Exclude<ToolChunk, string> {
  return typeof chunk === "string" ? { type: "text", text: chunk } : chunk
}

export function toolChunkText(chunk: ToolChunk): string {
  const normalized = normalizeToolChunk(chunk)
  return normalized.type === "text" ? normalized.text : ""
}

export function validateToolArguments(schemaJson: string, args: Record<string, unknown>): { error?: string; repaired: boolean } {
  let schema: Record<string, unknown>
  try { schema = JSON.parse(schemaJson) as Record<string, unknown> } catch { return { error: "invalid tool schema", repaired: false } }
  const state = { repaired: false }
  const wrapper = { root: args }
  const error = validateValue(schema, wrapper, "root", "$", state)
  return { error, repaired: state.repaired }
}

function validateValue(
  schema: Record<string, unknown>,
  parent: any,
  key: string | number,
  path: string,
  state: { repaired: boolean },
): string | undefined {
  let value = parent[key]
  const expectedType = schema.type

  // 1. 类型自动规整 (Auto-cast)
  if (typeof expectedType === "string") {
    if (expectedType === "boolean") {
      if (value === "true") {
        parent[key] = true
        value = true
        state.repaired = true
      } else if (value === "false") {
        parent[key] = false
        value = false
        state.repaired = true
      }
    } else if (expectedType === "number" || expectedType === "integer") {
      if (typeof value === "string") {
        const num = Number(value)
        if (!Number.isNaN(num)) {
          if (expectedType === "integer") {
            if (Number.isInteger(num)) {
              parent[key] = num
              value = num
              state.repaired = true
            }
          } else {
            parent[key] = num
            value = num
            state.repaired = true
          }
        }
      }
    }
  }

  // 2. 补默认值 (Default Injection)
  if (expectedType === "object") {
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      return `${path} must be object`
    }
    const properties = (schema.properties as Record<string, Record<string, unknown>> | undefined) ?? {}
    for (const [propKey, childSchema] of Object.entries(properties)) {
      if (!(propKey in value)) {
        if ("default" in childSchema) {
          value[propKey] = childSchema.default
          state.repaired = true
        }
      }
    }
  }

  // 3. 校验并递归
  if (typeof expectedType === "string") {
    if (expectedType === "object") {
      if (!value || typeof value !== "object" || Array.isArray(value)) return `${path} must be object`
      const obj = value as Record<string, unknown>

      // 3a. 裁剪多余字段
      const properties = (schema.properties as Record<string, Record<string, unknown>> | undefined) ?? {}
      const allowedKeys = new Set(Object.keys(properties))
      for (const objKey of Object.keys(obj)) {
        if (!allowedKeys.has(objKey)) {
          delete obj[objKey]
          state.repaired = true
        }
      }

      for (const required of (schema.required as string[] | undefined) ?? []) {
        if (!(required in obj)) return `${path}.${required} is required`
      }
      for (const [propKey, child] of Object.entries(properties)) {
        if (propKey in obj) {
          const err = validateValue(child, obj, propKey, `${path}.${propKey}`, state)
          if (err) return err
        }
      }
    } else if (expectedType === "array") {
      if (!Array.isArray(value)) return `${path} must be array`
      const arr = value as unknown[]
      const itemsSchema = schema.items as Record<string, unknown> | undefined
      if (itemsSchema) {
        for (let i = 0; i < arr.length; i++) {
          const err = validateValue(itemsSchema, arr, i, `${path}[${i}]`, state)
          if (err) return err
        }
      }
    } else if (expectedType === "string") {
      if (typeof value !== "string") return `${path} must be string`
    } else if (expectedType === "number") {
      if (typeof value !== "number" || Number.isNaN(value)) return `${path} must be number`
    } else if (expectedType === "integer") {
      if (!Number.isInteger(value)) return `${path} must be integer`
    } else if (expectedType === "boolean") {
      if (typeof value !== "boolean") return `${path} must be boolean`
    }
  } else if (path === "$" && (!value || typeof value !== "object" || Array.isArray(value))) {
    return `${path} must be object`
  }
  if (Array.isArray(schema.enum) && !schema.enum.includes(value)) return `${path} must be one of enum values`
  return undefined
}

export async function executeTools(
  calls: { id: string; name: string; arguments: string }[],
  registry: Map<string, RegisteredTool>,
): Promise<ToolResult[]> {
  return Promise.all(calls.map(async c => {
    const t = registry.get(c.name)
    if (!t) return { callId: c.id, output: `unknown tool: ${c.name}`, isError: true }
    try {
      const args = JSON.parse(c.arguments || "{}") as Record<string, unknown>
      const validation = validateToolArguments(t.schema.parameters, args)
      if (validation.error) return { callId: c.id, output: `invalid arguments: ${validation.error}`, isError: true }
      const output = await t.execute(args)
      if (isAsyncIterable<ToolChunk>(output)) {
        let combined = ""
        for await (const chunk of output) combined += toolChunkText(chunk)
        return { callId: c.id, output: combined, isError: false }
      }
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
