/**
 * Load GAIA smoke tasks or an external dataset JSON.
 */

import { readFileSync } from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

import { gradeGaia } from "./grade.mjs"
import { mkGaiaTools } from "./tools.mjs"

const __dir = path.dirname(fileURLToPath(import.meta.url))

/**
 * @param {{ limit?: number, dataset?: string }} [opts]
 * @returns {import("../../../core/types.mjs").CapTask[]}
 */
export function loadGaiaTasks(opts = {}) {
  const raw = opts.dataset
    ? JSON.parse(readFileSync(opts.dataset, "utf8"))
    : JSON.parse(readFileSync(path.join(__dir, "smoke-tasks.json"), "utf8"))

  const list = Array.isArray(raw) ? raw : (raw.tasks ?? raw.data ?? [])
  /** @type {import("../../../core/types.mjs").CapTask[]} */
  const tasks = list.map((item, i) => ({
    id: String(item.id ?? `gaia-${i + 1}`),
    goal: String(item.goal ?? item.Question ?? item.question ?? ""),
    category: item.category ?? (item.Level != null ? `gaia.l${item.Level}` : "gaia"),
    expected: item.expected ?? item.FinalAnswer ?? item.final_answer ?? item.answer,
    meta: item.meta ?? { files: item.files, searchHits: item.searchHits, level: item.Level },
  }))
  const limit = opts.limit != null ? Math.max(0, Math.floor(opts.limit)) : tasks.length
  return tasks.slice(0, limit)
}

/** @returns {import("../../../core/types.mjs").CapAdapter} */
export function createGaiaAdapter() {
  return {
    id: "gaia",
    description: "GAIA-style smoke (tool use + normalized final-answer match)",
    loadTasks: loadGaiaTasks,
    mkTools: (task, sdk) => mkGaiaTools(task, sdk),
    grade: gradeGaia,
    maxTurns: 10,
    maxTokens: 3072,
    timeoutMs: 180_000,
  }
}
