/**
 * Self-Harness proposal validation + promotion (H3).
 *
 * `evaluate` runs a manifest against a task set through the adapter, repeating each task `repeats`
 * times and folding to a per-task majority pass; `passCount` is the number of tasks that pass. The
 * loop calls it once per split (held-in, held-out) for the baseline and for each candidate, and the
 * deltas drive `acceptanceRule` — the paper's conservative rule, verbatim: an edit is accepted only
 * when it does not regress EITHER split and improves at least one (Δ_in ≥ 0 ∧ Δ_ho ≥ 0 ∧ max > 0).
 *
 * `mergeAccepted` composes the accepted patches of a round into one promoted manifest. Two patches
 * may co-merge only when their target surfaces are disjoint; on a surface collision the higher
 * (Δ_in + Δ_ho) edit wins and the rest are recorded as `skipped_conflict`. The promoted manifest's
 * `parent` is normalized to the round-start manifest's digest so the lineage chains one node per round.
 *
 * @typedef {import("../../node/src/harness/manifest.js").HarnessManifest} HarnessManifest
 * @typedef {import("../../node/src/harness/manifest.js").HarnessPatch} HarnessPatch
 * @typedef {import("./adapters/fixture.mjs").TaskAdapter} TaskAdapter
 * @typedef {import("./adapters/fixture.mjs").Task} Task
 *
 * @typedef {Object} TaskResult
 * @property {string} taskId
 * @property {boolean} passed          Majority pass over `repeats`.
 * @property {number} passes           Passing repeats.
 * @property {number} repeats
 * @property {import("./evidence.mjs").Verdict} [verdict]  Verdict from the last repeat (for evidence).
 * @property {import("./trace-excerpt.mjs").EventEnvelope[]} [events]
 * @property {string} [termination]
 *
 * @typedef {Object} EvalResult
 * @property {number} passCount        Number of tasks passing by majority.
 * @property {TaskResult[]} results
 *
 * @typedef {Object} AcceptedEdit
 * @property {HarnessPatch} patch
 * @property {number} dIn
 * @property {number} dHo
 * @property {any} [decision]          Back-reference the loop uses to flip a conflict-skipped decision.
 */

import { loadSdk } from "../utils/sdk.mjs"

let _sdk
async function sdk() {
  return (_sdk ??= await loadSdk())
}

/**
 * Evaluate a manifest against a task set, repeating each task and folding to a majority pass.
 * @param {{ manifest: HarnessManifest, adapter: TaskAdapter, tasks: Task[], repeats?: number }} args
 * @returns {Promise<EvalResult>}
 */
export async function evaluate({ manifest, adapter, tasks, repeats = 1 }) {
  const reps = Math.max(1, Math.floor(repeats))
  /** @type {TaskResult[]} */
  const results = []
  let passCount = 0

  for (const task of tasks) {
    let passes = 0
    let last = null
    for (let r = 0; r < reps; r++) {
      // A repeat that throws (provider hiccup, adapter fault) counts as a failed run — conservative
      // for candidates, and it keeps one bad task from killing a whole evaluation sweep.
      try {
        last = await adapter.runTask(task, manifest)
      } catch (e) {
        last = {
          passed: false,
          verdict: { passed: false, overallScore: 0, feedback: `adapter_error: ${e.message ?? e}`, details: [] },
          events: [],
          termination: "adapter_error",
        }
      }
      if (last.passed) passes++
    }
    const passed = passes * 2 > reps // strict majority (ties fail)
    if (passed) passCount++
    results.push({
      taskId: task.id,
      passed,
      passes,
      repeats: reps,
      verdict: last?.verdict,
      events: last?.events,
      termination: last?.termination,
    })
  }

  return { passCount, results }
}

/**
 * The paper's conservative acceptance rule: no regression on either split, strict gain on at least one.
 * @param {number} dIn
 * @param {number} dHo
 * @returns {boolean}
 */
export function acceptanceRule(dIn, dHo) {
  return dIn >= 0 && dHo >= 0 && (dIn > 0 || dHo > 0)
}

/**
 * Compose a round's accepted edits into one promoted manifest.
 * @param {HarnessManifest} manifest   The round-start manifest.
 * @param {AcceptedEdit[]} accepted
 * @returns {Promise<{ manifest: HarnessManifest, merged: AcceptedEdit[], skipped: Array<AcceptedEdit & { reason: string }> }>}
 */
export async function mergeAccepted(manifest, accepted) {
  const { applyPatch, manifestDigest } = await sdk()

  // Rank by combined delta desc so the strongest edit claims a contested surface.
  const ranked = [...accepted].sort((a, b) => (b.dIn + b.dHo) - (a.dIn + a.dHo))
  const claimed = new Set()
  /** @type {AcceptedEdit[]} */
  const merged = []
  /** @type {Array<AcceptedEdit & { reason: string }>} */
  const skipped = []
  for (const edit of ranked) {
    const surface = edit.patch.targetSurface
    if (claimed.has(surface)) {
      skipped.push({ ...edit, reason: "skipped_conflict" })
      continue
    }
    claimed.add(surface)
    merged.push(edit)
  }

  let out = manifest
  for (const edit of merged) {
    out = applyPatch(out, edit.patch) // never mutates its input
  }
  // Normalize lineage to the round boundary: one manifest node per round.
  if (merged.length > 0) out.parent = manifestDigest(manifest)

  return { manifest: out, merged, skipped }
}
