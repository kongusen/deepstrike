/**
 * Tolerant JSON extraction for LLM replies (H3 miner/proposer helper).
 *
 * Models wrap JSON in prose or ```json fences. `firstJsonValue` strips a fenced block if present,
 * tries a straight parse, and otherwise scans for the first balanced `{...}` or `[...]` value —
 * string-aware, so braces inside string literals don't confuse the depth counter. It throws when no
 * parseable JSON value is found, letting callers implement their own retry/discard policy.
 */

/** Remove a single ```json … ``` (or ``` … ```) fence, returning its body; else the input trimmed. */
function stripFence(text) {
  const s = String(text ?? "")
  const fenced = s.match(/```(?:json)?\s*([\s\S]*?)```/i)
  return (fenced ? fenced[1] : s).trim()
}

/**
 * Parse the first JSON object or array value found in `text`.
 * @param {string} text
 * @returns {unknown}
 */
export function firstJsonValue(text) {
  const body = stripFence(text)
  try {
    return JSON.parse(body)
  } catch {
    /* fall through to balanced scan */
  }

  const objAt = body.indexOf("{")
  const arrAt = body.indexOf("[")
  let start = -1
  if (objAt === -1) start = arrAt
  else if (arrAt === -1) start = objAt
  else start = Math.min(objAt, arrAt)
  if (start === -1) throw new Error("no JSON value found in text")

  const open = body[start]
  const close = open === "{" ? "}" : "]"
  let depth = 0
  let inStr = false
  let esc = false
  for (let i = start; i < body.length; i++) {
    const ch = body[i]
    if (inStr) {
      if (esc) esc = false
      else if (ch === "\\") esc = true
      else if (ch === '"') inStr = false
      continue
    }
    if (ch === '"') inStr = true
    else if (ch === open) depth++
    else if (ch === close) {
      depth--
      if (depth === 0) return JSON.parse(body.slice(start, i + 1))
    }
  }
  throw new Error("unbalanced JSON value in text")
}
