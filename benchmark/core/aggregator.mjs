/**
 * Aggregator: per-session raw metrics + events → MetricSet.
 *
 * Each scenario run = N sessions (one per task). We compute per-session aggregates first, then mean
 * + stdev across sessions for the MetricSet. This lets us report a stable mean and a 2σ
 * significance threshold even with small N — the dwell adapter does the same; this version is the
 * first-party path for runner-produced metrics.
 *
 * @typedef {import("./metrics.mjs").MetricSet} MetricSet
 * @typedef {import("./metrics.mjs").MetricValue} MetricValue
 * @typedef {import("./metrics.mjs").MetricMode} MetricMode
 *
 * @typedef {Object} SessionRecord
 * @property {string} sessionId
 * @property {string} taskId
 * @property {Array<{
 *   turn: number,
 *   toolsExposed: number,
 *   toolsCalled: number,
 *   activeSkill?: string,
 *   inputTokens: number,
 *   outputTokens?: number,
 *   cacheReadTokens: number,
 *   cacheCreationTokens: number,
 * }>} turnMetrics
 * @property {Array<{ seq: number, event: any }>} events
 * @property {number} wallMs
 * @property {string} finalStatus
 * @property {boolean} [passed]
 *
 * @typedef {Object} BuildMetricSetOpts
 * @property {string} scenarioId
 * @property {string} variantId
 * @property {string} provider
 * @property {string} model
 * @property {MetricMode} mode
 * @property {SessionRecord[]} sessions
 * @property {string} [timestamp]
 * @property {string} [notes]
 * @property {Record<string, any>} [pricing]
 * @property {(args: { events: any[], turnMetrics: any[] }) => Record<string, number>} [mechanismHook]
 */

/** @param {BuildMetricSetOpts} opts @returns {MetricSet} */
export function buildMetricSet(opts) {
  const { sessions, mode } = opts
  if (sessions.length === 0) throw new Error("buildMetricSet: no sessions")

  const perSession = sessions.map(s => aggregateSession(s, opts.mechanismHook))
  const samples = perSession.length

  const totalInput = sumField(perSession, "inputTokens")
  const totalOutput = sumField(perSession, "outputTokens")
  const totalCacheRead = sumField(perSession, "cacheReadTokens")
  const totalCacheCreate = sumField(perSession, "cacheCreationTokens")
  const totalTurns = sumField(perSession, "turnsUsed")

  const tokensPerTurn = perSession.map(s => s.tokensPerTurn)
  const cacheHitRate = perSession.map(s => s.cacheHitRate)
  const wallMs = perSession.map(s => s.wallMs)
  const msPerTurn = perSession.map(s => (s.turnsUsed > 0 ? s.wallMs / s.turnsUsed : 0))
  const peakInputTokens = perSession.map(s => s.peakInputTokens)
  const compressions = perSession.map(s => s.compressions)
  const successRate = perSession.map(s => (s.passed === true ? 1 : s.passed === false ? 0 : 0.5))
  const successKnown = perSession.some(s => s.passed !== undefined)

  const dollars = computeDollars({
    pricing: opts.pricing,
    provider: opts.provider,
    model: opts.model,
    inputTokens: totalInput,
    outputTokens: totalOutput,
    cacheReadTokens: totalCacheRead,
    cacheCreationTokens: totalCacheCreate,
  })

  // Mechanism layer: collect all hook outputs across sessions, mean+stdev per key.
  /** @type {Record<string, number[]>} */
  const mechAccum = {}
  for (const s of perSession) {
    for (const [k, v] of Object.entries(s.mechanismOut)) {
      (mechAccum[k] ??= []).push(v)
    }
  }

  /** @type {MetricSet} */
  const set = {
    schema: "deepstrike-bench/v0",
    meta: {
      scenarioId: opts.scenarioId,
      variantId: opts.variantId,
      provider: opts.provider,
      model: opts.model,
      mode,
      samples,
      timestamp: opts.timestamp ?? new Date().toISOString(),
      sessionCount: samples,
      turnCount: totalTurns,
      notes: opts.notes,
    },
    cost: {
      inputTokens: mv(totalInput, "tokens", mode, samples),
      outputTokens: totalOutput > 0 ? mv(totalOutput, "tokens", mode, samples) : undefined,
      cacheReadTokens: mv(totalCacheRead, "tokens", mode, samples),
      cacheCreationTokens: mv(totalCacheCreate, "tokens", mode, samples),
      cacheHitRate: meanStdev(cacheHitRate, "ratio", mode),
      tokensPerTurn: meanStdev(tokensPerTurn, "tokens", mode),
      ...(dollars !== null ? { dollars: mv(roundDollar(dollars), "$", mode, samples) } : {}),
    },
    latency: {
      wallMs: meanStdev(wallMs, "ms", mode),
      msPerTurn: meanStdev(msPerTurn, "ms", mode),
      turnsToDone: meanStdev(perSession.map(s => s.turnsUsed), "count", mode),
    },
    quality: {
      ...(successKnown
        ? { successRate: meanStdev(successRate, "ratio", mode) }
        : {}),
    },
    contextHealth: {
      peakInputTokens: meanStdev(peakInputTokens, "tokens", mode),
      compressions: meanStdev(compressions, "count", mode),
    },
    mechanism: {
      ...Object.fromEntries(
        Object.entries(mechAccum).map(([k, vs]) => [k, meanStdev(vs, undefined, mode)]),
      ),
    },
  }
  return set
}

/** @param {SessionRecord} s @param {BuildMetricSetOpts["mechanismHook"]} hook */
function aggregateSession(s, hook) {
  const m = s.turnMetrics
  const inputTokens = sumField(m, "inputTokens")
  const outputTokens = sumField(m, "outputTokens")
  const cacheReadTokens = sumField(m, "cacheReadTokens")
  const cacheCreationTokens = sumField(m, "cacheCreationTokens")
  const turnsUsed = m.length
  const tokensPerTurn = turnsUsed > 0 ? inputTokens / turnsUsed : 0
  const cacheHitRate = inputTokens > 0 ? cacheReadTokens / inputTokens : 0
  const peakInputTokens = Math.max(0, ...m.map(t => t.inputTokens || 0))
  const compressions = s.events.filter(e => e.event?.kind === "compressed").length

  const mechanismOut = hook ? safeHook(hook, { events: s.events, turnMetrics: m }) : {}

  return {
    inputTokens,
    outputTokens,
    cacheReadTokens,
    cacheCreationTokens,
    turnsUsed,
    tokensPerTurn,
    cacheHitRate,
    peakInputTokens,
    compressions,
    wallMs: s.wallMs,
    passed: s.passed,
    mechanismOut,
  }
}

function safeHook(hook, args) {
  try {
    const out = hook(args)
    if (!out || typeof out !== "object") return {}
    const result = {}
    for (const [k, v] of Object.entries(out)) {
      if (typeof v === "number" && isFinite(v)) result[k] = v
    }
    return result
  } catch {
    return {}
  }
}

function sumField(arr, key) { return arr.reduce((s, x) => s + (Number(x[key]) || 0), 0) }
function mean(arr) { return arr.length ? arr.reduce((s, x) => s + x, 0) / arr.length : 0 }
function stdev(arr, m) {
  if (arr.length < 2) return 0
  const variance = arr.reduce((s, x) => s + (x - m) ** 2, 0) / (arr.length - 1)
  return Math.sqrt(variance)
}

/** @returns {MetricValue} */
function meanStdev(arr, unit, mode) {
  const m = mean(arr)
  const sd = stdev(arr, m)
  /** @type {MetricValue} */
  const out = { value: round(m), mode, samples: arr.length, stdev: round(sd) }
  if (unit) out.unit = unit
  return out
}

/** @returns {MetricValue} — caller is responsible for rounding (integers pass through). */
function mv(value, unit, mode, samples) {
  return { value, unit, mode, samples }
}

function round(n) { return Math.round(n * 100) / 100 }
function roundDollar(n) { return Math.round(n * 1_000_000) / 1_000_000 }

function computeDollars({ pricing, provider, model, inputTokens, outputTokens, cacheReadTokens, cacheCreationTokens }) {
  if (!pricing) return null
  const key = `${provider}:${model}`
  const p = pricing[key]
  if (!p) return null
  let total = 0
  const uncachedInput = Math.max(0, inputTokens - cacheReadTokens - cacheCreationTokens)
  if (p.inputPerM != null) total += (uncachedInput / 1_000_000) * p.inputPerM
  if (p.cacheReadPerM != null) total += (cacheReadTokens / 1_000_000) * p.cacheReadPerM
  if (p.cacheWritePerM != null && cacheCreationTokens > 0) {
    total += (cacheCreationTokens / 1_000_000) * p.cacheWritePerM
  }
  if (p.outputPerM != null && outputTokens > 0) total += (outputTokens / 1_000_000) * p.outputPerM
  return total
}
