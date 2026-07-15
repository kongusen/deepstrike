/**
 * Build + render CapReport.
 */

import { writeFileSync } from "node:fs"
import path from "node:path"

/**
 * @param {{
 *   suite: string,
 *   provider: string,
 *   model: string,
 *   startedAt: string,
 *   finishedAt: string,
 *   results: import("./types.mjs").CapResult[],
 *   notes?: string,
 * }} args
 * @returns {import("./types.mjs").CapReport}
 */
export function buildReport(args) {
  const results = args.results ?? []
  const taskCount = results.length
  const passedCount = results.filter(r => r.grade?.passed).length
  const meanScore = taskCount === 0
    ? 0
    : results.reduce((s, r) => s + (Number(r.grade?.score) || 0), 0) / taskCount
  return {
    schema: "deepstrike-capability-report/v0",
    suite: args.suite,
    provider: args.provider,
    model: args.model,
    startedAt: args.startedAt,
    finishedAt: args.finishedAt,
    taskCount,
    passedCount,
    accuracy: taskCount === 0 ? 0 : passedCount / taskCount,
    meanScore,
    results,
    ...(args.notes ? { notes: args.notes } : {}),
  }
}

/** @param {import("./types.mjs").CapReport} report @param {string} outDir */
export function writeReport(report, outDir) {
  const fp = path.join(outDir, "report.json")
  writeFileSync(fp, JSON.stringify(report, null, 2))
  return fp
}

/** @param {import("./types.mjs").CapReport} report @param {NodeJS.WritableStream} [out] */
export function renderReportSummary(report, out = process.stdout) {
  const pct = (report.accuracy * 100).toFixed(1)
  out.write("══════════════════════════════════════════════════════════════\n")
  out.write(`  capability ${report.suite}  ·  ${report.provider}/${report.model}\n`)
  out.write(`  accuracy ${pct}%  (${report.passedCount}/${report.taskCount})  ·  meanScore ${report.meanScore.toFixed(3)}\n`)
  out.write("══════════════════════════════════════════════════════════════\n")
  for (const r of report.results) {
    const mark = r.grade?.passed ? "PASS" : "FAIL"
    const reason = r.grade?.reason ? ` — ${r.grade.reason}` : ""
    const err = r.error ? ` [err: ${r.error.slice(0, 80)}]` : ""
    out.write(`  [${mark}] ${r.taskId}  score=${(r.grade?.score ?? 0).toFixed(2)}  ${r.status}${reason}${err}\n`)
  }
  out.write("══════════════════════════════════════════════════════════════\n")
}
