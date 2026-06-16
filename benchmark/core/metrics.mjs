/**
 * MetricSet — the single shape every benchmark run produces.
 *
 * Five layers (cost / latency / quality / contextHealth / mechanism) cover every mechanism in the
 * §7 matrix. Each metric carries its own `mode` so a `replay` cost can sit next to a `live` quality
 * value in the same MetricSet, and the diff renderer can mark live values as `n±σ`. This is BM0a:
 * just the schema — producers (BM0b runner, adapters/dwell-report) fill in what they have.
 *
 * @typedef {"replay" | "live" | "mixed"} MetricMode
 *
 * @typedef {Object} MetricValue
 * @property {number} value           Point estimate. For live with samples>1, the mean.
 * @property {string} [unit]          Display-only unit ("tokens" / "ms" / "$" / "ratio" / "count").
 * @property {MetricMode} mode        How this value was measured. Drives diff significance handling.
 * @property {number} [samples]       For live: independent runs feeding this value. Absent ⇒ 1.
 * @property {number} [stdev]         For live with samples>1: standard deviation of the samples.
 *
 * @typedef {Object} MetricSetMeta
 * @property {string} scenarioId      Stable scenario id (e.g. "gating-dwell", "K01-rho").
 * @property {string} variantId       Variant identifier ("off" / "on" / "policy-A").
 * @property {string} provider        LLM provider id ("deepseek" / "openai" / "anthropic" / "minimax").
 * @property {string} model           Concrete model id at run time.
 * @property {MetricMode} mode        Aggregate run mode. "mixed" when both replay and live metrics live in this set.
 * @property {number} samples         Total samples (live=sessions, replay=1).
 * @property {string} timestamp       ISO timestamp the run finished at.
 * @property {number} sessionCount    Distinct sessions (a session = one runner.run call).
 * @property {number} turnCount       Total LLM turns observed across all sessions.
 * @property {string} [notes]         Free-form notes — adapter name, source files, caveats.
 *
 * @typedef {Object} MetricSet
 * @property {"deepstrike-bench/v0"} schema
 * @property {MetricSetMeta} meta
 * @property {Record<string, MetricValue | undefined>} cost            Tokens, cache split, $ from pricing.
 * @property {Record<string, MetricValue | undefined>} latency         Wall-clock and per-turn timing.
 * @property {Record<string, MetricValue | undefined>} quality         Criteria-based success + per-mechanism quality probes.
 * @property {Record<string, MetricValue | undefined>} contextHealth   Peak rho, compressions, paging.
 * @property {Record<string, MetricValue | undefined>} mechanism       Mechanism-specific (open).
 */

/** Layers the diff renderer iterates, in display order. */
export const LAYERS = ["cost", "latency", "quality", "contextHealth", "mechanism"]

/**
 * Synthesise a MetricValue. Used by adapters; not part of the schema.
 * @param {number} value
 * @param {Partial<Omit<MetricValue, "value">>} [opts]
 * @returns {MetricValue}
 */
export function mv(value, opts = {}) {
  return { value, mode: opts.mode ?? "live", ...opts }
}

/**
 * Validate a MetricSet shape at the boundary (loaded from JSON). Throws on structural failure;
 * returns the input so it can be chained.
 * @param {unknown} obj
 * @returns {MetricSet}
 */
export function assertMetricSet(obj) {
  if (!obj || typeof obj !== "object") throw new Error("MetricSet: not an object")
  const m = /** @type {Record<string, unknown>} */ (obj)
  if (m.schema !== "deepstrike-bench/v0") {
    throw new Error(`MetricSet: unknown schema ${JSON.stringify(m.schema)}`)
  }
  if (!m.meta || typeof m.meta !== "object") throw new Error("MetricSet: missing meta")
  for (const layer of LAYERS) {
    if (m[layer] === undefined) throw new Error(`MetricSet: missing layer ${layer}`)
    if (typeof m[layer] !== "object") throw new Error(`MetricSet: layer ${layer} not an object`)
  }
  return /** @type {MetricSet} */ (obj)
}
