/**
 * Load BFCL smoke tasks or an external dataset JSON.
 */

import { readFileSync } from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { gradeBfcl } from "./grade.mjs"
import { mkBfclTools } from "./tools.mjs"

const __dir = path.dirname(fileURLToPath(import.meta.url))

/**
 * @param {{ limit?: number, dataset?: string }} [opts]
 * @returns {import("../../../core/types.mjs").CapTask[]}
 */
export function loadBfclTasks(opts = {}) {
  const raw = opts.dataset
    ? JSON.parse(readFileSync(opts.dataset, "utf8"))
    : JSON.parse(readFileSync(path.join(__dir, "smoke-tasks.json"), "utf8"))

  const list = Array.isArray(raw) ? raw : (raw.tasks ?? raw.data ?? [])
  /** @type {import("../../../core/types.mjs").CapTask[]} */
  const tasks = list.map((item, i) => normalizeTask(item, i))
  const limit = opts.limit != null ? Math.max(0, Math.floor(opts.limit)) : tasks.length
  return tasks.slice(0, limit)
}

/** @param {any} item @param {number} i */
function normalizeTask(item, i) {
  if (item.goal && item.id) {
    return {
      id: String(item.id),
      goal: String(item.goal),
      category: item.category ?? "bfcl",
      functions: item.functions ?? item.tools ?? [],
      expected: item.expected ?? item.ground_truth ?? [],
      meta: item.meta,
    }
  }
  return {
    id: String(item.id ?? item.idx ?? `bfcl-${i + 1}`),
    goal: String(item.goal ?? item.question ?? item.query ?? ""),
    category: item.category ?? "bfcl.external",
    functions: item.functions ?? item.function ?? item.tools ?? [],
    expected: item.expected ?? item.ground_truth ?? item.answer ?? [],
    meta: item,
  }
}

/** @returns {import("../../../core/types.mjs").CapAdapter} */
export function createBfclAdapter() {
  return {
    id: "bfcl",
    description: "Berkeley Function-Calling Leaderboard style smoke (tool name + args match)",
    loadTasks: loadBfclTasks,
    mkTools: (task, sdk) => mkBfclTools(task, sdk),
    grade: gradeBfcl,
    maxTurns: 8,
    maxTokens: 2048,
    timeoutMs: 120_000,
  }
}
