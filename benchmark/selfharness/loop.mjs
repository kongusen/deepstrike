/**
 * Self-Harness propose→validate→promote loop (H3) — the outer optimization ring.
 *
 * Each round: evaluate the current manifest on BOTH splits (baseline) → build a held-in evidence
 * bundle from the failures → mine mechanisms → keep only addressable clusters as proposer targets →
 * propose ≤K patches → for each candidate re-evaluate both splits and apply the conservative
 * acceptance rule → merge the accepted edits into one promoted manifest → persist the lineage. The
 * promoted manifest becomes the next round's current.
 *
 * The whole run is deterministic: no timestamps, no randomness enter the persisted records — a
 * manifest's identity is its content digest and artifact freshness is carried by file mtime. Held-out
 * leakage is prevented structurally: only the held-in bundle ever reaches the miner/proposer, and
 * non-addressable clusters are filtered before the proposer sees them.
 *
 * Lineage on disk (`lineageDir`, default `.harness-lab/`), laned by scope × modelProfile so parallel
 * runs never collide (`"default"` substitutes for an absent axis — the two are ORTHOGONAL, never joined):
 *   - `<scope>/<modelProfile>/<digest>.json`  one file per persisted manifest (seed + every promotion)
 *   - `<scope>/<modelProfile>/rounds.jsonl`   one line per round: { round, scope, baseline, ... }
 * The scope is derived from the seed manifest — the single source of truth, so no caller-supplied
 * option can disagree with the manifest it stamps.
 *
 * @typedef {import("../../node/src/harness/manifest.js").HarnessManifest} HarnessManifest
 * @typedef {import("./adapters/fixture.mjs").TaskAdapter} TaskAdapter
 *
 * @typedef {Object} RoundRecord
 * @property {number} round
 * @property {{ heldIn: number, heldOut: number }} baseline
 * @property {Array<{ surface: string, op: string, targetCluster: string }>} proposals
 * @property {Array<{ patch?: unknown, reason: string }>} discarded
 * @property {Array<{ surface: string, targetCluster?: string, accepted: boolean, deltaIn?: number, deltaHo?: number, reason?: string }>} decisions
 * @property {string} promotedDigest
 * @property {string | null} parent
 *
 * @typedef {Object} LoopResult
 * @property {HarnessManifest} finalManifest
 * @property {RoundRecord[]} trajectory
 */

import { appendFileSync, mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"

import { loadSdk } from "../utils/sdk.mjs"
import { extractFailureRecord, buildEvidenceBundle } from "./evidence.mjs"
import { mineMechanisms } from "./miner.mjs"
import { propose } from "./proposer.mjs"
import { evaluate, acceptanceRule, mergeAccepted } from "./validate.mjs"

let _sdk
async function sdk() {
  return (_sdk ??= await loadSdk())
}

/**
 * Run the self-harness loop.
 * @param {{
 *   seedManifest: HarnessManifest,
 *   adapter: TaskAdapter,
 *   heldIn: string[],
 *   heldOut: string[],
 *   rounds: number,
 *   k?: number,
 *   repeats?: number,
 *   complete: (prompt: string) => Promise<string>,
 *   lineageDir?: string,
 *   log?: (msg: string) => void,
 * }} args
 * @returns {Promise<LoopResult>}
 */
export async function selfHarnessLoop({
  seedManifest,
  adapter,
  heldIn,
  heldOut,
  rounds,
  k = 4,
  repeats = 1,
  complete,
  lineageDir = ".harness-lab",
  log = () => {},
}) {
  const { manifestDigest, applyPatch, validateManifest } = await sdk()

  // Fail fast AND guard the lineage path: validateManifest enforces the scope pattern before scope is
  // ever used as a directory segment, so a path-hostile seed scope can't escape lineageDir.
  validateManifest(seedManifest)
  const scope = seedManifest.scope // orthogonal to modelProfile; never concatenated

  const allTasks = await adapter.listTasks()
  const byId = new Map(allTasks.map(t => [t.id, t]))
  const resolve = ids => ids.map(id => {
    const t = byId.get(id)
    if (!t) throw new Error(`selfHarnessLoop: unknown task id "${id}"`)
    return t
  })
  const heldInTasks = resolve(heldIn)
  const heldOutTasks = resolve(heldOut)

  // <lineageDir>/<scope>/<modelProfile>/ — scope is pattern-validated above; modelProfile is advisory
  // metadata so it is sanitized (not rejected) to a path token, letting a slash-bearing model id lane
  // without escaping. Absent axis ⇒ "default".
  const laneDir = path.join(lineageDir, scope ?? "default", laneSegment(seedManifest.modelProfile))
  mkdirSync(laneDir, { recursive: true })
  const roundsLogPath = path.join(laneDir, "rounds.jsonl")
  writeFileSync(roundsLogPath, "") // fresh run — deterministic content, freshness via mtime

  let current = seedManifest
  writeManifest(laneDir, current, manifestDigest) // persist the seed

  /** @type {RoundRecord[]} */
  const trajectory = []
  /** @type {import("./evidence.mjs").PreviousAttempt[]} */
  const previousAttempts = []

  for (let round = 0; round < rounds; round++) {
    log(`── round ${round} · harness ${manifestDigest(current).slice(0, 12)} ──`)

    // 1. Baseline on both splits.
    const baseIn = await evaluate({ manifest: current, adapter, tasks: heldInTasks, repeats })
    const baseOut = await evaluate({ manifest: current, adapter, tasks: heldOutTasks, repeats })

    // 2. Held-in evidence bundle.
    const records = baseIn.results.map(r => extractFailureRecord({
      taskId: r.taskId,
      events: r.events ?? [],
      verdict: r.verdict ?? { passed: r.passed, overallScore: r.passed ? 1 : 0, feedback: "", details: [] },
      criteria: byId.get(r.taskId)?.criteria ?? [],
      eventsPath: `${r.taskId}.events.json`,
      scope,
    }))
    /** @type {Record<string, any>} */
    const eventsByTask = {}
    for (const r of baseIn.results) if (!r.passed && r.events) eventsByTask[r.taskId] = r.events
    const bundle = buildEvidenceBundle({
      round,
      harnessDigest: manifestDigest(current),
      records,
      scope,
      previousAttempts: [...previousAttempts],
      eventsByTask,
    })

    // 3. Mine mechanisms; keep addressable clusters only as proposer targets.
    const attributions = await mineMechanisms({ bundle, complete })
    const addressable = new Map(attributions.filter(a => a.addressable).map(a => [a.clusterKey, a]))
    const minedClusters = bundle.clusters
      .filter(c => addressable.has(c.key))
      .map(c => {
        const a = addressable.get(c.key)
        return { ...c, mechanism: a.mechanism, mechanismReasoning: a.reasoning }
      })
    const minedBundle = { ...bundle, clusters: minedClusters }

    // 4. Propose ≤K patches (held-in + addressable only — no held-out can reach here).
    const { patches, discarded } = await propose({ bundle: minedBundle, manifest: current, k, complete })

    // 5. Validate each candidate against both splits.
    /** @type {RoundRecord["decisions"]} */
    const decisions = []
    /** @type {import("./validate.mjs").AcceptedEdit[]} */
    const accepted = []
    for (const patch of patches) {
      let candidate
      try {
        candidate = applyPatch(current, patch)
      } catch (e) {
        decisions.push({ surface: patch.targetSurface, accepted: false, reason: `apply_failed: ${e.message ?? e}` })
        continue
      }
      // Paper §3.4: a proposal that fails execution before a valid evaluation result is REJECTED,
      // never allowed to kill the loop (e.g. a candidate the kernel refuses at ConfigureRun).
      let candIn, candOut
      try {
        candIn = await evaluate({ manifest: candidate, adapter, tasks: heldInTasks, repeats })
        candOut = await evaluate({ manifest: candidate, adapter, tasks: heldOutTasks, repeats })
      } catch (e) {
        decisions.push({ surface: patch.targetSurface, accepted: false, reason: `eval_failed: ${e.message ?? e}` })
        continue
      }
      const dIn = candIn.passCount - baseIn.passCount
      const dHo = candOut.passCount - baseOut.passCount
      const ok = acceptanceRule(dIn, dHo)
      const decision = { surface: patch.targetSurface, targetCluster: patch.targetCluster, accepted: ok, deltaIn: dIn, deltaHo: dHo }
      decisions.push(decision)
      if (ok) accepted.push({ patch, dIn, dHo, decision })
      previousAttempts.push({
        surface: patch.targetSurface,
        summary: patch.expectedEffect ?? patch.rationale ?? "",
        accepted: ok,
        deltaIn: dIn,
        deltaHo: dHo,
      })
    }

    // 6. Merge + promote.
    const { manifest: promotedRaw, merged, skipped } = await mergeAccepted(current, accepted)
    for (const s of skipped) if (s.decision) { s.decision.accepted = false; s.decision.reason = "skipped_conflict" }

    let promoted = promotedRaw
    if (merged.length > 0) {
      const top = merged[0]
      promoted = {
        ...promoted,
        audit: {
          round,
          createdBy: "proposer",
          targetCluster: top.patch.targetCluster,
          rationale: top.patch.rationale,
          deltaHeldIn: top.dIn,
          deltaHeldOut: top.dHo,
        },
      }
      writeManifest(laneDir, promoted, manifestDigest)
    }

    // 7. Lineage record.
    const roundRecord = {
      round,
      scope: scope ?? "default",
      baseline: { heldIn: baseIn.passCount, heldOut: baseOut.passCount },
      proposals: patches.map(p => ({ surface: p.targetSurface, op: p.op, targetCluster: p.targetCluster })),
      discarded,
      decisions,
      promotedDigest: manifestDigest(promoted),
      parent: promoted.parent,
    }
    appendFileSync(roundsLogPath, JSON.stringify(roundRecord) + "\n")
    trajectory.push(roundRecord)
    current = promoted
  }

  return { finalManifest: current, trajectory }
}

/**
 * Coerce a modelProfile into a single path-safe directory token. Unlike `scope` (a validated data
 * contract), modelProfile is advisory metadata and may legitimately carry separators (e.g. a
 * `vendor/model` id), so it is sanitized rather than rejected. Absent / empty ⇒ "default".
 */
function laneSegment(raw) {
  if (raw === undefined || raw === null || raw === "") return "default"
  const token = String(raw).replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 64)
  return token.length > 0 ? token : "default"
}

/** Persist a manifest as `<digest>.json`; returns the digest. */
function writeManifest(dir, manifest, digestFn) {
  const digest = digestFn(manifest)
  writeFileSync(path.join(dir, `${digest}.json`), JSON.stringify(manifest, null, 2))
  return digest
}
