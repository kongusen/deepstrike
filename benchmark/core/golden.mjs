/**
 * Golden baselines for regression detection.
 *
 * A "golden" is a frozen MetricSet annotated with a tolerance policy. `checkGolden` compares a
 * new MetricSet against the golden and reports each metric as PASS / FAIL — failures bubble up to
 * a non-zero CLI exit code so CI can gate merges.
 *
 * Two tension axes the tolerance policy resolves:
 *   - **Mechanical vs. quality metrics.** `cost.inputTokens` under replay is deterministic
 *     bit-for-bit; `quality.successRate` under live varies session-to-session even on identical
 *     scenarios. So mechanical metrics get strict tolerances and quality metrics get loose ones.
 *   - **Mode (live vs. replay).** Replay is deterministic across the board; live is sampled.
 *     Replay defaults to "essentially exact" (0.001%), live to the per-layer defaults below.
 *
 * Lookup order for a metric's tolerance:
 *   1. golden.tolerance.metrics["{layer}.{key}"]  (explicit per-metric)
 *   2. golden.tolerance.layers[layer]             (explicit per-layer)
 *   3. mode-default for that layer                (mode === "replay" ⇒ strict; else live defaults)
 *
 * @typedef {import("./metrics.mjs").MetricSet} MetricSet
 *
 * @typedef {Object} Tolerance
 * @property {number} [absPct]   Allowed |Δ|/|golden| × 100 percentage. Falls back to absAbs when |golden| ≈ 0.
 * @property {number} [absAbs]   Allowed absolute |Δ|. Wins over absPct when both are set.
 *
 * @typedef {Object} GoldenTolerancePolicy
 * @property {Partial<Record<"cost"|"latency"|"quality"|"contextHealth"|"mechanism", Tolerance>>} [layers]
 * @property {Record<string, Tolerance>} [metrics]   Key form: "{layer}.{key}", e.g. "cost.inputTokens".
 *
 * @typedef {Object} GoldenKey
 * @property {string} scenarioId
 * @property {string} variantId
 * @property {string} provider
 * @property {string} model
 * @property {string} mode
 *
 * @typedef {Object} Golden
 * @property {"deepstrike-bench-golden/v0"} schema
 * @property {GoldenKey} key
 * @property {string} captured                   ISO timestamp.
 * @property {GoldenTolerancePolicy} [tolerance]
 * @property {MetricSet} metricSet
 *
 * @typedef {Object} MetricFailure
 * @property {string} layer
 * @property {string} key
 * @property {number | null} golden
 * @property {number | null} current
 * @property {number | null} deltaAbs
 * @property {number | null} deltaPct
 * @property {Tolerance} tolerance
 * @property {"missing" | "exceeds_tolerance" | "added"} reason
 *
 * @typedef {Object} CheckResult
 * @property {boolean} passed
 * @property {number} totalChecked
 * @property {MetricFailure[]} failures
 * @property {Array<{ layer: string, key: string, current: number }>} extras  Metrics in current set but absent from golden.
 * @property {GoldenKey} key
 * @property {string} captured
 */

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs"
import path from "node:path"

import { LAYERS } from "./metrics.mjs"

// ── defaults ────────────────────────────────────────────────────────────────

/** @type {Tolerance} */
const REPLAY_STRICT = { absPct: 0.001 }

const LIVE_DEFAULTS = /** @type {const} */ ({
  cost: { absPct: 10 },
  latency: { absPct: 50 },
  quality: { absAbs: 0.25 },
  contextHealth: { absPct: 10 },
  mechanism: { absPct: 5 },
})

// ── path helpers ────────────────────────────────────────────────────────────

/**
 * Canonical filesystem path for a golden, given the root baselines dir + key.
 * @param {string} baselinesDir
 * @param {GoldenKey} key
 */
export function goldenPath(baselinesDir, key) {
  return path.join(
    baselinesDir,
    key.scenarioId,
    `${key.variantId}.${key.provider}.${key.model}.${key.mode}.json`,
  )
}

/** @param {MetricSet} set @returns {GoldenKey} */
export function keyFromMetricSet(set) {
  return {
    scenarioId: set.meta.scenarioId,
    variantId: set.meta.variantId,
    provider: set.meta.provider,
    model: set.meta.model,
    mode: set.meta.mode,
  }
}

// ── save / load ─────────────────────────────────────────────────────────────

/**
 * Write `metricSet` as a golden at `goldenPath(baselinesDir, keyFromMetricSet(metricSet))`.
 * Preserves any tolerance overrides from a prior golden at the same path.
 *
 * @param {{ metricSet: MetricSet, baselinesDir: string, tolerance?: GoldenTolerancePolicy }} args
 * @returns {string} Absolute path written.
 */
export function saveGolden(args) {
  const key = keyFromMetricSet(args.metricSet)
  const target = goldenPath(args.baselinesDir, key)
  mkdirSync(path.dirname(target), { recursive: true })

  // Preserve human-edited tolerance from an existing golden unless the caller supplied a new one.
  let inheritedTolerance
  if (!args.tolerance && existsSync(target)) {
    try {
      const prior = JSON.parse(readFileSync(target, "utf8"))
      inheritedTolerance = prior.tolerance
    } catch { /* ignore */ }
  }

  /** @type {Golden} */
  const golden = {
    schema: "deepstrike-bench-golden/v0",
    key,
    captured: args.metricSet.meta.timestamp ?? new Date().toISOString(),
    ...(args.tolerance ?? inheritedTolerance ? { tolerance: args.tolerance ?? inheritedTolerance } : {}),
    metricSet: args.metricSet,
  }
  writeFileSync(target, JSON.stringify(golden, null, 2))
  return target
}

/** @param {string} p @returns {Golden} */
export function loadGolden(p) {
  const raw = JSON.parse(readFileSync(p, "utf8"))
  if (raw?.schema !== "deepstrike-bench-golden/v0") {
    throw new Error(`${p}: not a deepstrike-bench-golden/v0 file (schema=${JSON.stringify(raw?.schema)})`)
  }
  if (!raw.metricSet?.meta || !raw.key) {
    throw new Error(`${p}: golden missing metricSet or key`)
  }
  return /** @type {Golden} */ (raw)
}

// ── check ───────────────────────────────────────────────────────────────────

/**
 * Compare `current` against `golden`. Iterates every metric in golden across all 5 layers,
 * applies the tolerance lookup chain, and reports failures.
 *
 * @param {MetricSet} current
 * @param {Golden} golden
 * @returns {CheckResult}
 */
export function checkGolden(current, golden) {
  /** @type {MetricFailure[]} */
  const failures = []
  /** @type {Array<{ layer: string, key: string, current: number }>} */
  const extras = []
  let totalChecked = 0
  const mode = golden.metricSet.meta.mode

  for (const layer of LAYERS) {
    const gLayer = /** @type {Record<string, any>} */ (golden.metricSet[layer] ?? {})
    const cLayer = /** @type {Record<string, any>} */ (current[layer] ?? {})

    for (const key of Object.keys(gLayer)) {
      const gMetric = gLayer[key]
      if (!gMetric || typeof gMetric.value !== "number") continue
      totalChecked++
      const cMetric = cLayer[key]
      const tolerance = resolveTolerance(layer, key, mode, golden.tolerance)
      if (!cMetric || typeof cMetric.value !== "number") {
        failures.push({ layer, key, golden: gMetric.value, current: null, deltaAbs: null, deltaPct: null, tolerance, reason: "missing" })
        continue
      }
      const delta = cMetric.value - gMetric.value
      const absDelta = Math.abs(delta)
      const pct = gMetric.value !== 0 ? (delta / Math.abs(gMetric.value)) * 100 : null
      if (!withinTolerance(gMetric.value, cMetric.value, tolerance)) {
        failures.push({
          layer, key,
          golden: gMetric.value,
          current: cMetric.value,
          deltaAbs: round(absDelta, 6),
          deltaPct: pct === null ? null : round(pct, 2),
          tolerance,
          reason: "exceeds_tolerance",
        })
      }
    }

    for (const key of Object.keys(cLayer)) {
      if (gLayer[key] === undefined && cLayer[key]?.value !== undefined) {
        extras.push({ layer, key, current: cLayer[key].value })
      }
    }
  }

  return {
    passed: failures.length === 0,
    totalChecked,
    failures,
    extras,
    key: golden.key,
    captured: golden.captured,
  }
}

/** @returns {Tolerance} */
function resolveTolerance(layer, key, mode, policy) {
  const metricKey = `${layer}.${key}`
  if (policy?.metrics?.[metricKey]) return policy.metrics[metricKey]
  if (policy?.layers?.[layer]) return policy.layers[layer]
  if (mode === "replay") return REPLAY_STRICT
  return LIVE_DEFAULTS[layer] ?? { absPct: 10 }
}

/**
 * @param {number} goldenValue
 * @param {number} currentValue
 * @param {Tolerance} tol
 */
function withinTolerance(goldenValue, currentValue, tol) {
  const delta = Math.abs(currentValue - goldenValue)
  if (tol.absAbs !== undefined) {
    return delta <= tol.absAbs
  }
  if (tol.absPct !== undefined) {
    const base = Math.abs(goldenValue)
    if (base < 1e-9) {
      // Golden is effectively zero — fall back to an absolute floor: 1% of an arbitrary scale or
      // exact zero if the metric is genuinely a ratio. Use 1e-6 as a tiny floor.
      return delta <= 1e-6
    }
    return (delta / base) * 100 <= tol.absPct
  }
  return true
}

function round(n, decimals) {
  const f = 10 ** decimals
  return Math.round(n * f) / f
}

// ── render ──────────────────────────────────────────────────────────────────

/**
 * Render a CheckResult as a fixed-width table with PASS / FAIL per failure and an `extras` tail.
 * @param {CheckResult} result
 * @returns {string}
 */
export function renderGoldenCheck(result) {
  const lines = []
  const k = result.key
  const headline = `golden check · ${k.scenarioId} · ${k.variantId} · ${k.provider} · ${k.model} · ${k.mode}`
  const bar = "═".repeat(Math.max(82, headline.length + 4))
  lines.push(bar)
  lines.push("  " + headline)
  lines.push(`  captured ${result.captured}`)
  lines.push(bar)

  if (result.failures.length === 0) {
    lines.push(`  ✅ PASS — ${result.totalChecked} metric(s) within tolerance`)
  } else {
    lines.push(`  ❌ FAIL — ${result.failures.length} of ${result.totalChecked} metric(s) outside tolerance:`)
    lines.push("")
    for (const f of result.failures) {
      const tol = fmtTolerance(f.tolerance)
      if (f.reason === "missing") {
        lines.push(`    ✗ ${f.layer}.${f.key}  MISSING from current set  (golden=${fmtNum(f.golden)})`)
      } else {
        const sign = f.deltaPct !== null && f.deltaPct > 0 ? "+" : ""
        const dpct = f.deltaPct === null ? "n/a" : `${sign}${f.deltaPct}%`
        lines.push(`    ✗ ${f.layer}.${f.key}  ${fmtNum(f.golden)} → ${fmtNum(f.current)}  (Δ=${fmtNum(f.deltaAbs)}, ${dpct}; tolerance ${tol})`)
      }
    }
  }

  if (result.extras.length > 0) {
    lines.push("")
    lines.push(`  ℹ  ${result.extras.length} new metric(s) in current set (not in golden):`)
    for (const e of result.extras.slice(0, 10)) {
      lines.push(`     · ${e.layer}.${e.key} = ${fmtNum(e.current)}`)
    }
    if (result.extras.length > 10) lines.push(`     · … and ${result.extras.length - 10} more`)
  }

  lines.push(bar)
  return lines.join("\n")
}

function fmtTolerance(t) {
  if (t.absAbs !== undefined) return `|Δ| ≤ ${t.absAbs}`
  if (t.absPct !== undefined) return `≤ ${t.absPct}%`
  return "n/a"
}

function fmtNum(n) {
  if (n === null) return "—"
  if (!isFinite(n)) return String(n)
  if (Math.abs(n) >= 1000) return n.toLocaleString("en-US", { maximumFractionDigits: 0 })
  if (Math.abs(n) >= 1) return String(Math.round(n * 100) / 100)
  if (n === 0) return "0"
  return String(Math.round(n * 100000) / 100000)
}
