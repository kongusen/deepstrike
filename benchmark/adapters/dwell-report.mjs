/**
 * Adapter: dwell-report.json → MetricSet.
 *
 * The dwell example (node/examples/tool-gating-dwell.mjs) emits a `dwell-report.json` with its own
 * shape. Until producers learn to write MetricSet directly (BM0b), this adapter folds an existing
 * dwell run into the unified schema so BM2's diff CLI can consume it.
 *
 * Variant id is taken from the CLI flag — the dwell script doesn't self-identify, so the caller
 * passes "on"/"off" via `bench diff --variant-id`.
 *
 * @typedef {import("../core/metrics.mjs").MetricSet} MetricSet
 * @typedef {import("../core/metrics.mjs").MetricValue} MetricValue
 * @typedef {import("../core/metrics.mjs").MetricMode} MetricMode
 *
 * @typedef {Object} PricingEntry
 * @property {number | null} inputPerM
 * @property {number | null} outputPerM
 * @property {number | null} cacheReadPerM
 * @property {number | null} cacheWritePerM
 *
 * @typedef {Record<string, PricingEntry | undefined>} PricingTable
 *
 * @typedef {Object} DwellAdapterOpts
 * @property {string} variantId
 * @property {string} [scenarioId]
 * @property {MetricMode} [mode]      Defaults to "live" — dwell today is always live.
 * @property {PricingTable} [pricing]
 * @property {string} [timestamp]
 */

/**
 * @param {any} raw          parsed dwell-report.json
 * @param {DwellAdapterOpts} opts
 * @returns {MetricSet}
 */
export function dwellReportToMetricSet(raw, opts) {
  if (!raw?.report || !raw?.config || !Array.isArray(raw?.metrics)) {
    throw new Error("dwell-report: missing report/config/metrics fields")
  }
  const r = raw.report
  const cfg = raw.config
  const mode = opts.mode ?? "live"
  const sessions = r.sessions || 1

  const perSession = groupBySession(raw.metrics)
  const tokensPerTurnPerSession = perSession.map(turns =>
    turns.length ? turns.reduce((s, t) => s + t.inputTokens, 0) / turns.length : 0,
  )
  const tokensPerTurnMean = mean(tokensPerTurnPerSession)
  const tokensPerTurnStdev = stdev(tokensPerTurnPerSession, tokensPerTurnMean)

  const exposurePerSession = perSession.map(turns =>
    turns.length ? turns.reduce((s, t) => s + t.toolsExposed, 0) / turns.length : 0,
  )
  const exposureMean = mean(exposurePerSession)
  const exposureStdev = stdev(exposurePerSession, exposureMean)

  const totalInput = r.cache.totalInputTokens
  const priceKey = `${cfg.provider}:${cfg.model}`
  const price = opts.pricing?.[priceKey]
  const dollars = price ? computeDollars(r, price) : null

  /** @type {MetricSet} */
  const set = {
    schema: "deepstrike-bench/v0",
    meta: {
      scenarioId: opts.scenarioId ?? "gating-dwell",
      variantId: opts.variantId,
      provider: cfg.provider,
      model: cfg.model,
      mode,
      samples: sessions,
      timestamp: opts.timestamp ?? new Date().toISOString(),
      sessionCount: sessions,
      turnCount: r.turnsTotal,
      notes: `imported from dwell-report (${perSession.length} sessions)${price ? "" : ", pricing n/a"}`,
    },
    cost: {
      inputTokens: build(totalInput, "tokens", mode, sessions),
      cacheReadTokens: build(r.cache.cacheReadTokens, "tokens", mode, sessions),
      cacheCreationTokens: build(r.cache.cacheCreationTokens, "tokens", mode, sessions),
      cacheHitRate: build(r.cache.cacheHitRate, "ratio", mode, sessions),
      tokensPerTurn: { value: round(tokensPerTurnMean), unit: "tokens", mode, samples: sessions, stdev: round(tokensPerTurnStdev) },
      ...(dollars !== null ? { dollars: build(dollars, "$", mode, sessions) } : {}),
    },
    latency: {},
    quality: {},
    contextHealth: {
      maxCacheReadInOneTurn: build(r.cache.maxCacheReadInOneTurn, "tokens", mode, sessions),
      boundaryP: build(r.cache.boundaryP, "tokens", mode, sessions),
    },
    mechanism: {
      avgToolsExposed: { value: round(exposureMean), unit: "count", mode, samples: sessions, stdev: round(exposureStdev) },
      avgToolsCalled: build(r.exposure.avgToolsCalled, "count", mode, sessions),
      calledToExposedRatio: build(r.exposure.calledToExposedRatio, "ratio", mode, sessions),
      skillActivations: build(r.skillActivations, "count", mode, sessions),
      dwellMean: build(r.dwell.mean, "turns", mode, sessions),
      dwellMedian: build(r.dwell.median, "turns", mode, sessions),
      dwellP90: build(r.dwell.p90, "turns", mode, sessions),
    },
  }
  return set
}

/**
 * @param {number} value
 * @param {string} unit
 * @param {MetricMode} mode
 * @param {number} samples
 * @returns {MetricValue}
 */
function build(value, unit, mode, samples) {
  return { value: round(value), unit, mode, samples }
}

function groupBySession(metrics) {
  /** @type {Record<string, any[]>} */
  const by = {}
  for (const m of metrics) (by[m.sessionId] ??= []).push(m)
  return Object.values(by)
}

function computeDollars(report, p) {
  if (p.inputPerM === null && p.cacheReadPerM === null) return null
  let total = 0
  const uncached = Math.max(
    0,
    report.cache.totalInputTokens - report.cache.cacheReadTokens - report.cache.cacheCreationTokens,
  )
  if (p.inputPerM !== null) total += (uncached / 1_000_000) * p.inputPerM
  if (p.cacheReadPerM !== null) total += (report.cache.cacheReadTokens / 1_000_000) * p.cacheReadPerM
  if (p.cacheWritePerM !== null && report.cache.cacheCreationTokens > 0) {
    total += (report.cache.cacheCreationTokens / 1_000_000) * p.cacheWritePerM
  }
  return total
}

function mean(arr) { return arr.length ? arr.reduce((s, x) => s + x, 0) / arr.length : 0 }
function stdev(arr, m) {
  if (arr.length < 2) return 0
  const variance = arr.reduce((s, x) => s + (x - m) ** 2, 0) / (arr.length - 1)
  return Math.sqrt(variance)
}
function round(n) { return Math.round(n * 100) / 100 }
