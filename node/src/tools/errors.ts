import type { ToolExecContext, RegisteredTool } from "./index.js"
import { tool } from "./index.js"

/**
 * Stable JSON shape returned to the model by `safeTool`. The model can branch on `code` instead
 * of pattern-matching a free-form string. `hint` is the self-correcting affordance — a short
 * suggestion ("call document_outline first") that the agent can follow on its own.
 */
export interface ToolEnvelopeOk<T = unknown> {
  success: true
  data?: T
}

export interface ToolEnvelopeFail {
  success: false
  code: string
  error: string
  hint?: string
}

export type ToolEnvelope<T = unknown> = ToolEnvelopeOk<T> | ToolEnvelopeFail

/**
 * Error class understood by `safeTool` and by the runtime's error-aware serialization. A throw
 * of `ToolError` produces `{ success:false, code, error, hint? }`; a plain `Error` (or anything
 * with a string `.code`/`.hint`) is honored too, so existing code that already sets `code` on a
 * custom Error keeps working without migration.
 */
export class ToolError extends Error {
  code: string
  hint?: string
  constructor(message: string, opts: { code?: string; hint?: string; cause?: unknown } = {}) {
    super(message, opts.cause !== undefined ? { cause: opts.cause } : undefined)
    this.name = "ToolError"
    this.code = opts.code ?? "internal"
    if (opts.hint !== undefined) this.hint = opts.hint
  }
}

export function ok<T>(data?: T): ToolEnvelopeOk<T> {
  return data === undefined ? { success: true } : { success: true, data }
}

export function fail(code: string, error: string, hint?: string): ToolEnvelopeFail {
  return hint === undefined ? { success: false, code, error } : { success: false, code, error, hint }
}

function isEnvelope(v: unknown): v is ToolEnvelope {
  return typeof v === "object" && v !== null && "success" in (v as Record<string, unknown>) &&
    typeof (v as { success: unknown }).success === "boolean"
}

/**
 * Error-aware serialization for tool-execution error paths. Replaces `String(err)` at the
 * sites that hand the model (or the host's stream) a failure message.
 *
 * - `Error` with no extra fields → `err.message` (clean, no `"Error: "` prefix).
 * - `Error` carrying `code` / `hint` / `cause` → JSON `{message, name?, code?, hint?, cause?}`.
 * - Plain objects → `JSON.stringify(...)` (replaces the old `"[object Object]"`).
 * - Primitives / null / undefined → `String(...)` (unchanged).
 */
export function formatToolError(err: unknown): string {
  if (err == null) return String(err)
  if (typeof err === "string") return err
  if (err instanceof Error) {
    const anyErr = err as Error & { code?: unknown; hint?: unknown; cause?: unknown }
    const code = anyErr.code
    const hint = anyErr.hint
    const cause = anyErr.cause
    if (code === undefined && hint === undefined && cause === undefined) {
      return err.message || err.name || "Error"
    }
    const payload: Record<string, unknown> = { message: err.message }
    if (err.name && err.name !== "Error") payload.name = err.name
    if (code !== undefined) payload.code = code
    if (hint !== undefined) payload.hint = hint
    if (cause !== undefined) payload.cause = cause instanceof Error ? cause.message : cause
    try { return JSON.stringify(payload) } catch { return err.message || err.name || "Error" }
  }
  if (typeof err === "object") {
    try { return JSON.stringify(err) } catch { return Object.prototype.toString.call(err) }
  }
  return String(err)
}

/**
 * `tool()` equivalent that wraps the body in a try/catch and returns a stable
 * `{success, code, error, hint?}` JSON envelope to the model:
 *
 * - body returns plain data → `{success:true, data}`
 * - body returns an envelope (via `ok()`/`fail()`) → passed through
 * - body throws `ToolError` → `{success:false, code, error, hint?}`
 * - body throws any other `Error` → `{success:false, code: error.code ?? "internal", error: error.message}`
 * - body throws a non-Error → `{success:false, code:"internal", error: formatToolError(...)}`
 *
 * The classic `tool()` factory is unchanged. `safeTool` is opt-in: import and switch one tool at
 * a time. Designed for the consumer-side pattern users had to hand-roll to escape the legacy
 * `String(err)` foot-gun.
 */
export function safeTool<T = unknown>(
  name: string,
  description: string,
  parameters: Record<string, unknown>,
  fn: (args: Record<string, unknown>, ctx?: ToolExecContext) => Promise<ToolEnvelope<T> | T> | ToolEnvelope<T> | T,
): RegisteredTool {
  const wrapped = async (args: Record<string, unknown>, ctx?: ToolExecContext): Promise<string> => {
    try {
      const result = await fn(args, ctx)
      if (isEnvelope(result)) return JSON.stringify(result)
      return JSON.stringify(ok(result))
    } catch (err) {
      if (err instanceof ToolError) {
        return JSON.stringify(fail(err.code, err.message || err.name, err.hint))
      }
      if (err instanceof Error) {
        const anyErr = err as Error & { code?: unknown; hint?: unknown }
        const code = typeof anyErr.code === "string" ? anyErr.code : "internal"
        const hint = typeof anyErr.hint === "string" ? anyErr.hint : undefined
        return JSON.stringify(fail(code, err.message || err.name || "Error", hint))
      }
      return JSON.stringify(fail("internal", formatToolError(err)))
    }
  }
  return tool(name, description, parameters, wrapped)
}
