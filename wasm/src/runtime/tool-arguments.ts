/** Tool-call argument text is model-authored. A call whose arguments are not a JSON object (a
 *  truncated stream, prose, an array) must reach the executor as-is so it fails as an invalid
 *  call — rewriting it to `{}` would run the tool with empty arguments the model never wrote. */

/** Canonical text for a model-authored argument string: re-serialized when it is a JSON object,
 *  `{}` when empty, and the raw text verbatim otherwise. */
export function toolArgumentsText(raw: string | undefined): string {
  const text = raw ?? ""
  if (text.trim() === "") return "{}"
  const parsed = tryParse(text)
  return isJsonObject(parsed) ? JSON.stringify(parsed) : text
}

/** Wire value handed to the kernel: the object when the text is one, the raw string otherwise
 *  (the kernel carries arguments opaquely and hands the same value back on `execute_tools`). */
export function toolArgumentsToWire(raw: string | undefined): Record<string, unknown> | string {
  const text = raw ?? ""
  if (text.trim() === "") return {}
  const parsed = tryParse(text)
  return isJsonObject(parsed) ? parsed : text
}

/** Inverse of {@link toolArgumentsToWire}: kernel-carried arguments back to SDK text. */
export function toolArgumentsFromWire(value: unknown): string {
  if (typeof value === "string") return value
  return JSON.stringify(value ?? {})
}

/** `{ rawArguments }` for a streamed `tool_call` event whose text is not a JSON object. */
export function malformedToolArguments(raw: string | undefined): { rawArguments?: string } {
  const text = raw ?? ""
  if (text.trim() === "") return {}
  return isJsonObject(tryParse(text)) ? {} : { rawArguments: text }
}

export type ParsedToolArguments =
  | { ok: true; args: Record<string, unknown> }
  | { ok: false; error: string }

/** Parse arguments for execution. Anything but a JSON object is an invalid call. */
export function parseToolCallArguments(raw: string | undefined): ParsedToolArguments {
  const text = raw ?? ""
  if (text.trim() === "") return { ok: true, args: {} }
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch (err) {
    return { ok: false, error: `arguments are not valid JSON (${(err as Error).message})` }
  }
  if (!isJsonObject(parsed)) return { ok: false, error: "arguments must be a JSON object" }
  return { ok: true, args: parsed }
}

function tryParse(text: string): unknown {
  try { return JSON.parse(text) } catch { return undefined }
}

function isJsonObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
