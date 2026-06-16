/**
 * Pure diff: take baseline and variant MetricSets, emit one DiffRow per metric union.
 *
 * Significance:
 *   - replay mode (both sides): any non-zero Δ is "significant" (replay is deterministic).
 *   - live mode with stdev on both sides: |Δ| > 2 × max(σ_baseline, σ_variant) ⇒ significant.
 *   - Mixed modes or missing stdev: significance is null (caller decides).
 *
 * No IO, no formatting — that lives in render.mjs.
 *
 * @typedef {import("./metrics.mjs").MetricSet} MetricSet
 * @typedef {import("./metrics.mjs").MetricValue} MetricValue
 *
 * @typedef {Object} DiffRow
 * @property {"cost" | "latency" | "quality" | "contextHealth" | "mechanism"} layer
 * @property {string} key
 * @property {MetricValue | null} baseline
 * @property {MetricValue | null} variant
 * @property {number | null} deltaAbs
 * @property {number | null} deltaPct
 * @property {boolean | null} significant     null = cannot decide (live without stdev, or one side missing).
 *
 * @typedef {Object} DiffResult
 * @property {string} scenarioId
 * @property {{variantId: string, mode: string, samples: number}} baseline
 * @property {{variantId: string, mode: string, samples: number}} variant
 * @property {string[]} warnings
 * @property {DiffRow[]} rows
 */

import { LAYERS } from "./metrics.mjs"

/**
 * @param {MetricSet} baseline
 * @param {MetricSet} variant
 * @returns {DiffResult}
 */
export function diff(baseline, variant) {
  /** @type {string[]} */
  const warnings = []
  if (baseline.meta.scenarioId !== variant.meta.scenarioId) {
    warnings.push(`scenarioId mismatch: baseline=${baseline.meta.scenarioId} variant=${variant.meta.scenarioId}`)
  }
  if (baseline.meta.provider !== variant.meta.provider) {
    warnings.push(`provider mismatch: baseline=${baseline.meta.provider} variant=${variant.meta.provider} — Δ% only, absolutes not comparable`)
  }
  if (baseline.meta.model !== variant.meta.model) {
    warnings.push(`model mismatch: baseline=${baseline.meta.model} variant=${variant.meta.model}`)
  }

  /** @type {DiffRow[]} */
  const rows = []
  for (const layer of LAYERS) {
    const bLayer = baseline[layer] ?? {}
    const vLayer = variant[layer] ?? {}
    const keys = new Set([...Object.keys(bLayer), ...Object.keys(vLayer)])
    for (const key of [...keys].sort()) {
      const b = bLayer[key] ?? null
      const v = vLayer[key] ?? null
      rows.push(buildRow(layer, key, b, v))
    }
  }

  return {
    scenarioId: baseline.meta.scenarioId,
    baseline: {
      variantId: baseline.meta.variantId,
      mode: baseline.meta.mode,
      samples: baseline.meta.samples,
    },
    variant: {
      variantId: variant.meta.variantId,
      mode: variant.meta.mode,
      samples: variant.meta.samples,
    },
    warnings,
    rows,
  }
}

/**
 * @param {DiffRow["layer"]} layer
 * @param {string} key
 * @param {MetricValue | null} b
 * @param {MetricValue | null} v
 * @returns {DiffRow}
 */
function buildRow(layer, key, b, v) {
  if (b === null || v === null) {
    return { layer, key, baseline: b, variant: v, deltaAbs: null, deltaPct: null, significant: null }
  }
  const deltaAbs = round(v.value - b.value)
  const deltaPct = b.value !== 0 ? round(((v.value - b.value) / Math.abs(b.value)) * 100, 1) : null
  return { layer, key, baseline: b, variant: v, deltaAbs, deltaPct, significant: decideSignificance(b, v, deltaAbs) }
}

/**
 * @param {MetricValue} b
 * @param {MetricValue} v
 * @param {number} deltaAbs
 * @returns {boolean | null}
 */
function decideSignificance(b, v, deltaAbs) {
  if (b.mode === "replay" && v.mode === "replay") return deltaAbs !== 0
  if (b.stdev !== undefined && v.stdev !== undefined) {
    const sigma = Math.max(b.stdev, v.stdev)
    return Math.abs(deltaAbs) > 2 * sigma
  }
  return null
}

function round(n, decimals = 4) {
  const f = 10 ** decimals
  return Math.round(n * f) / f
}
