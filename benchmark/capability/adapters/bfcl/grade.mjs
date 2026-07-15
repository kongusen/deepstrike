/**
 * BFCL-style deterministic grader: match expected tool name(s) + normalized args.
 * Does NOT use LLM-judge.
 */

/**
 * @param {unknown} v
 * @returns {unknown}
 */
export function normalizeValue(v) {
  if (typeof v === "string") {
    let s = v.trim()
    // Soften common surface variants for smoke scoring.
    if (/^(english|en)$/i.test(s)) s = "English"
    if (/^(french|fr|français|francais)$/i.test(s)) s = "French"
    if (/^new\s*york$/i.test(s)) s = "New York"
    if (/^downtown\s+seattle$/i.test(s)) s = "downtown Seattle"
    if (/^sqrt\s*\(\s*144\s*\)$/i.test(s) || s === "√144" || s === "144**0.5" || s === "144^0.5") {
      s = "sqrt(144)"
    }
    return s
  }
  if (typeof v === "number") return v
  if (typeof v === "boolean") return v
  if (Array.isArray(v)) return v.map(normalizeValue)
  if (v && typeof v === "object") {
    /** @type {Record<string, unknown>} */
    const out = {}
    for (const k of Object.keys(v).sort()) {
      out[k] = normalizeValue(/** @type {any} */ (v)[k])
    }
    return out
  }
  return v
}

/**
 * @param {Record<string, unknown>} a
 * @param {Record<string, unknown>} b
 */
export function argsMatch(a, b) {
  const na = /** @type {Record<string, unknown>} */ (normalizeValue(a ?? {}))
  const nb = /** @type {Record<string, unknown>} */ (normalizeValue(b ?? {}))
  // Expected keys must be present and equal; extra actual keys are allowed
  // (models often add optional fields).
  for (const key of Object.keys(nb)) {
    if (!(key in na)) return false
    if (!deepEqual(na[key], nb[key])) return false
  }
  return true
}

/** @param {unknown} a @param {unknown} b */
function deepEqual(a, b) {
  if (a === b) return true
  if (typeof a === "number" && typeof b === "number") {
    return Math.abs(a - b) < 1e-9
  }
  if (typeof a !== typeof b) {
    // Coerce numeric strings ("100" vs 100) for smoke softness.
    if ((typeof a === "string" || typeof a === "number") && (typeof b === "string" || typeof b === "number")) {
      const na = Number(a)
      const nb = Number(b)
      if (!Number.isNaN(na) && !Number.isNaN(nb)) return Math.abs(na - nb) < 1e-9
    }
    return false
  }
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false
    return a.every((x, i) => deepEqual(x, b[i]))
  }
  if (a && b && typeof a === "object") {
    const ak = Object.keys(/** @type {object} */ (a)).sort()
    const bk = Object.keys(/** @type {object} */ (b)).sort()
    if (ak.length !== bk.length) return false
    return ak.every((k, i) => k === bk[i] && deepEqual(/** @type {any} */ (a)[k], /** @type {any} */ (b)[k]))
  }
  if (typeof a === "string" && typeof b === "string") {
    return a.toLowerCase() === b.toLowerCase()
  }
  return false
}

/**
 * @param {{
 *   task: import("../../../core/types.mjs").CapTask,
 *   finalText: string,
 *   toolCalls: import("../../../core/types.mjs").CapToolCall[],
 *   status: string,
 * }} args
 * @returns {import("../../../core/types.mjs").CapGrade}
 */
export function gradeBfcl(args) {
  const expected = /** @type {Array<{ name: string, arguments: Record<string, unknown> }>} */ (
    args.task.expected ?? []
  )
  if (!expected.length) {
    return { passed: false, score: 0, reason: "no expected calls in task" }
  }
  if (!args.toolCalls.length) {
    return { passed: false, score: 0, reason: "model made no tool calls", detail: { finalText: args.finalText?.slice(0, 200) } }
  }

  // For each expected call, find a matching actual call (order-insensitive; first unused match).
  const used = new Set()
  let matched = 0
  /** @type {string[]} */
  const misses = []

  for (const exp of expected) {
    let found = -1
    for (let i = 0; i < args.toolCalls.length; i++) {
      if (used.has(i)) continue
      const act = args.toolCalls[i]
      if (act.name !== exp.name) continue
      if (!argsMatch(act.arguments ?? {}, exp.arguments ?? {})) continue
      found = i
      break
    }
    if (found >= 0) {
      used.add(found)
      matched++
    } else {
      misses.push(`${exp.name}(${JSON.stringify(exp.arguments)})`)
    }
  }

  const score = matched / expected.length
  const passed = matched === expected.length
  return {
    passed,
    score,
    reason: passed
      ? `matched ${matched}/${expected.length} expected calls`
      : `matched ${matched}/${expected.length}; missing: ${misses.join("; ")}`,
    detail: {
      expected,
      actual: args.toolCalls,
      matched,
    },
  }
}
