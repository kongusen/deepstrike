/**
 * GAIA-style normalized string match against Final answer: line or full text.
 */

/**
 * @param {string} text
 * @returns {string}
 */
export function extractFinalAnswer(text) {
  const s = String(text ?? "")
  const m = s.match(/Final\s*answer\s*:\s*(.+)/i)
  if (m) return m[1].trim().split(/\r?\n/)[0].trim()
  // Fallback: last non-empty line
  const lines = s.split(/\r?\n/).map(l => l.trim()).filter(Boolean)
  return lines.length ? lines[lines.length - 1] : ""
}

/** @param {string} s */
export function normalizeAnswer(s) {
  return String(s ?? "")
    .trim()
    .replace(/^["'`]+|["'`]+$/g, "")
    .replace(/[.,;:!?]+$/g, "")
    .replace(/\s+/g, " ")
    .toLowerCase()
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
export function gradeGaia(args) {
  const expected = normalizeAnswer(String(args.task.expected ?? ""))
  if (!expected) {
    return { passed: false, score: 0, reason: "task has no expected answer" }
  }
  const extracted = extractFinalAnswer(args.finalText)
  const actual = normalizeAnswer(extracted)
  const full = normalizeAnswer(args.finalText)

  const exact = actual === expected || full === expected
  const contains = !exact && (actual.includes(expected) || full.includes(expected) || expected.includes(actual))
  const passed = exact || (contains && actual.length > 0 && actual.length <= expected.length * 3)
  const score = exact ? 1 : (passed ? 0.75 : 0)

  return {
    passed,
    score,
    reason: passed
      ? (exact ? `exact match: ${extracted}` : `fuzzy match: got "${extracted}" ≈ "${args.task.expected}"`)
      : `expected "${args.task.expected}", got "${extracted || "(empty)"}"`,
    detail: { expected: args.task.expected, extracted, toolCallCount: args.toolCalls.length },
  }
}
