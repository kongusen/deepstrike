// G3 structured output: a small, dependency-free JSON-Schema subset validator + helpers used by the
// workflow runner to enforce a node's `output_schema`. The kernel carries the schema verbatim (it is
// zero-I/O and never validates); enforcement lives here, SDK-side, where the agent output exists.
//
// Supported keywords (the common structured-output subset): `type` (object | array | string |
// number | integer | boolean | null), `required`, `properties` (recursive), `items` (recursive),
// `enum`. Unknown keywords are ignored rather than rejected — a permissive superset of these specs
// still validates, matching "instruct the model, then check the shape" rather than full JSON Schema.

export interface SchemaValidation {
  ok: boolean
  errors: string[]
}

type JsonSchema = Record<string, unknown>

function typeOfValue(v: unknown): string {
  if (v === null) return "null"
  if (Array.isArray(v)) return "array"
  return typeof v // "object" | "string" | "number" | "boolean"
}

function matchesType(v: unknown, t: string): boolean {
  switch (t) {
    case "integer":
      return typeof v === "number" && Number.isInteger(v)
    case "number":
      return typeof v === "number"
    case "object":
      return typeOfValue(v) === "object"
    default:
      return typeOfValue(v) === t
  }
}

/** Validate `value` against `schema` (the supported subset). `path` is for error messages. */
export function validateAgainstSchema(value: unknown, schema: JsonSchema, path = "$"): SchemaValidation {
  const errors: string[] = []

  const type = schema.type
  if (typeof type === "string" && !matchesType(value, type)) {
    errors.push(`${path}: expected ${type}, got ${typeOfValue(value)}`)
    return { ok: false, errors } // type mismatch ⇒ stop; deeper checks are meaningless
  }
  if (Array.isArray(type) && !type.some(t => typeof t === "string" && matchesType(value, t))) {
    errors.push(`${path}: expected one of [${type.join(", ")}], got ${typeOfValue(value)}`)
    return { ok: false, errors }
  }

  if (Array.isArray(schema.enum) && !schema.enum.some(e => e === value)) {
    errors.push(`${path}: value not in enum`)
  }

  if (typeOfValue(value) === "object") {
    const obj = value as Record<string, unknown>
    const required = Array.isArray(schema.required) ? (schema.required as string[]) : []
    for (const key of required) {
      if (!(key in obj)) errors.push(`${path}.${key}: required property missing`)
    }
    const properties = (schema.properties as Record<string, JsonSchema> | undefined) ?? {}
    for (const [key, sub] of Object.entries(properties)) {
      if (key in obj) {
        const r = validateAgainstSchema(obj[key], sub, `${path}.${key}`)
        if (!r.ok) errors.push(...r.errors)
      }
    }
  }

  if (typeOfValue(value) === "array" && schema.items && typeof schema.items === "object") {
    const items = schema.items as JsonSchema
    ;(value as unknown[]).forEach((el, i) => {
      const r = validateAgainstSchema(el, items, `${path}[${i}]`)
      if (!r.ok) errors.push(...r.errors)
    })
  }

  return { ok: errors.length === 0, errors }
}

/** The instruction appended to a node's goal so its agent produces schema-conforming JSON. */
export function schemaInstruction(schema: JsonSchema): string {
  return (
    "You MUST return ONLY a single JSON value that conforms to this JSON Schema, with no prose, " +
    "no markdown, and no code fences:\n" +
    JSON.stringify(schema)
  )
}

/** A stronger re-prompt for a retry after a validation failure. */
export function schemaRetryInstruction(schema: JsonSchema, errors: string[]): string {
  return (
    `${schemaInstruction(schema)}\n\nYour previous output did NOT conform: ${errors.join("; ")}. ` +
    "Return ONLY the corrected JSON value."
  )
}

/** Best-effort extraction of a JSON value from agent output (raw, fenced, or embedded). */
export function extractJsonValue(text: string): unknown {
  const trimmed = (text ?? "").trim()
  if (!trimmed) return undefined
  const tryParse = (s: string): unknown => {
    try {
      return JSON.parse(s)
    } catch {
      return undefined
    }
  }
  const whole = tryParse(trimmed)
  if (whole !== undefined) return whole

  const fence = trimmed.match(/```(?:json)?\s*([\s\S]*?)```/i)
  if (fence) {
    const fenced = tryParse(fence[1].trim())
    if (fenced !== undefined) return fenced
  }

  // Fall back to the first balanced {...} or [...] slice.
  for (const [open, close] of [["{", "}"], ["[", "]"]] as const) {
    const start = trimmed.indexOf(open)
    const end = trimmed.lastIndexOf(close)
    if (start !== -1 && end > start) {
      const slice = tryParse(trimmed.slice(start, end + 1))
      if (slice !== undefined) return slice
    }
  }
  return undefined
}
