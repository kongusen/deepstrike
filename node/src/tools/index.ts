import type { ToolChunk, ToolSchema, ToolResult } from "../types.js"
import { formatToolError } from "./errors.js"

/** M3/G4: the runtime context a tool may read when executing. Carries the working directory the tool
 *  should operate in — set to a sub-agent's git worktree for `isolation: "worktree"` nodes. A narrow,
 *  dependency-free shape; the execution plane's `RunContext` is structurally assignable to it.
 *
 *  `audit` is the "best-effort post-commit side-effect" channel: wrap an audit-log write,
 *  metrics emit, or any non-essential persistence in `await ctx.audit(label, () => store.write(...))`.
 *  If the side-effect throws, the failure is recorded as a `tool_audit_failed` stream event and
 *  the tool still completes successfully — avoiding the foot-gun where a transient audit-store
 *  outage flips an already-committed write into `isError: true` and triggers a duplicate retry. */
export interface ToolExecContext {
  cwd?: string
  audit?: (label: string, fn: () => Promise<void> | void) => Promise<void>
}

export interface RegisteredTool {
  schema: ToolSchema
  execute(args: Record<string, unknown>, ctx?: ToolExecContext): Promise<string> | AsyncIterable<ToolChunk>
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

export function streamingTool(
  name: string,
  description: string,
  parameters: Record<string, unknown>,
  fn: (args: Record<string, unknown>, ctx?: ToolExecContext) => AsyncIterable<ToolChunk>,
): RegisteredTool {
  return {
    schema: { name, description, parameters: JSON.stringify(parameters) },
    execute(args, ctx) { return fn(args, ctx) },
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
      return { callId: c.id, output: formatToolError(err), isError: true }
    }
  }))
}

/**
 * One-shot heuristic: detect when a streaming tool yielded text that *looks* like a failure
 * envelope. The runtime cannot block the tool from doing it, but we warn (once per tool) so
 * the author migrates to throwing — the canonical "streaming tool fails" path. Aligns with
 * the non-streaming tool() / safeTool() contract: failures throw, successes return data.
 */
const _warnedFailureShapes = new Set<string>()
export function maybeWarnFailureShapedChunk(toolName: string, deltaText: string): void {
  if (!deltaText || _warnedFailureShapes.has(toolName)) return
  const trimmed = deltaText.trim()
  if (trimmed.length < 2 || trimmed[0] !== "{") return
  let parsed: unknown
  try { parsed = JSON.parse(trimmed) } catch { return }
  if (typeof parsed !== "object" || parsed === null) return
  const obj = parsed as Record<string, unknown>
  const looksLikeFailure =
    obj.success === false ||
    obj.isError === true ||
    obj.is_error === true
  if (!looksLikeFailure) return
  _warnedFailureShapes.add(toolName)
  console.warn(
    `[deepstrike] streaming tool "${toolName}" yielded a failure-shaped chunk ` +
    `(success:false / isError:true). Streaming tools should fail by throwing; ` +
    `the runtime will catch and surface the error consistently. ` +
    `Returning a failure-shaped chunk is a foot-gun: the kernel still sees isError:false.`,
  )
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
