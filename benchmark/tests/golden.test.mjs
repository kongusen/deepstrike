/**
 * Golden module tests — uses Node's built-in test runner (no external deps).
 *
 * Run:  node --test benchmark/tests/golden.test.mjs
 */

import assert from "node:assert/strict"
import { test, describe } from "node:test"
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import path from "node:path"

import {
  saveGolden,
  loadGolden,
  checkGolden,
  goldenPath,
  keyFromMetricSet,
  renderGoldenCheck,
} from "../core/golden.mjs"

// ── helpers ─────────────────────────────────────────────────────────────────

function makeSet(overrides = {}) {
  return {
    schema: "deepstrike-bench/v0",
    meta: {
      scenarioId: "s1",
      variantId: "off",
      provider: "deepseek",
      model: "deepseek-chat",
      mode: "live",
      samples: 4,
      timestamp: "2026-06-16T00:00:00Z",
      sessionCount: 4,
      turnCount: 48,
      ...overrides.meta,
    },
    cost: {
      inputTokens: { value: 1000, mode: "live", unit: "tokens", samples: 4 },
      tokensPerTurn: { value: 250, mode: "live", unit: "tokens", samples: 4, stdev: 10 },
      cacheHitRate: { value: 0.75, mode: "live", unit: "ratio", samples: 4 },
      ...overrides.cost,
    },
    latency: { wallMs: { value: 1000, mode: "live", unit: "ms", samples: 4 }, ...overrides.latency },
    quality: { successRate: { value: 0.8, mode: "live", unit: "ratio", samples: 4 }, ...overrides.quality },
    contextHealth: { peakInputTokens: { value: 500, mode: "live", unit: "tokens", samples: 4 }, ...overrides.contextHealth },
    mechanism: { avgToolsExposed: { value: 28.62, mode: "live", unit: "count", samples: 4 }, ...overrides.mechanism },
  }
}

function withTempDir(fn) {
  const dir = mkdtempSync(path.join(tmpdir(), "bench-golden-test-"))
  try { return fn(dir) } finally { rmSync(dir, { recursive: true, force: true }) }
}

// ── path helpers ────────────────────────────────────────────────────────────

describe("goldenPath / keyFromMetricSet", () => {
  test("produces hierarchical path keyed by (variant, provider, model, mode)", () => {
    const set = makeSet()
    const key = keyFromMetricSet(set)
    assert.equal(key.scenarioId, "s1")
    assert.equal(key.variantId, "off")
    assert.equal(key.mode, "live")
    assert.equal(
      goldenPath("/base", key),
      "/base/s1/off.deepseek.deepseek-chat.live.json",
    )
  })
})

// ── save / load round-trip ──────────────────────────────────────────────────

describe("saveGolden / loadGolden", () => {
  test("round-trips a MetricSet through disk", () => {
    withTempDir(dir => {
      const set = makeSet()
      const p = saveGolden({ metricSet: set, baselinesDir: dir })
      const loaded = loadGolden(p)
      assert.equal(loaded.schema, "deepstrike-bench-golden/v0")
      assert.equal(loaded.metricSet.meta.scenarioId, "s1")
      assert.equal(loaded.metricSet.cost.inputTokens.value, 1000)
    })
  })

  test("preserves prior tolerance overrides on re-save (sticky human edits)", () => {
    withTempDir(dir => {
      const set = makeSet()
      const p = saveGolden({ metricSet: set, baselinesDir: dir })
      // human edits the file to tighten a metric:
      const golden = JSON.parse(readFileSync(p, "utf8"))
      golden.tolerance = { metrics: { "cost.cacheHitRate": { absAbs: 0.05 } } }
      writeFileSync(p, JSON.stringify(golden, null, 2))
      // re-save (no explicit tolerance arg) — should INHERIT the human edit:
      const set2 = makeSet({ cost: { inputTokens: { value: 1100, mode: "live", unit: "tokens", samples: 4 } } })
      saveGolden({ metricSet: set2, baselinesDir: dir })
      const reloaded = loadGolden(p)
      assert.deepEqual(reloaded.tolerance.metrics["cost.cacheHitRate"], { absAbs: 0.05 })
    })
  })

  test("rejects non-golden JSON", () => {
    withTempDir(dir => {
      const p = path.join(dir, "fake.json")
      writeFileSync(p, JSON.stringify({ not: "a golden" }))
      assert.throws(() => loadGolden(p), /not a deepstrike-bench-golden/)
    })
  })
})

// ── tolerance defaults ──────────────────────────────────────────────────────

describe("checkGolden — layer defaults", () => {
  test("PASS: exact match across all layers", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    const r = checkGolden(makeSet(), golden)
    assert.equal(r.passed, true)
    assert.equal(r.failures.length, 0)
    assert.equal(r.totalChecked, 7) // 3 cost + 1 latency + 1 quality + 1 contextHealth + 1 mechanism
  })

  test("live cost drift within 10% default → PASS", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    // inputTokens 1000 → 1080, that's +8% < 10% default
    const current = makeSet({ cost: { inputTokens: { value: 1080, mode: "live", unit: "tokens", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true, JSON.stringify(r.failures))
  })

  test("live cost drift over 10% default → FAIL", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    // inputTokens 1000 → 1200, that's +20% > 10% default
    const current = makeSet({ cost: { inputTokens: { value: 1200, mode: "live", unit: "tokens", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, false)
    const failure = r.failures.find(f => f.key === "inputTokens")
    assert.equal(failure.reason, "exceeds_tolerance")
    assert.equal(failure.deltaPct, 20)
  })

  test("live quality drift within absAbs 0.25 default → PASS", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    // successRate 0.8 → 0.6, |Δ|=0.2 < 0.25 default
    const current = makeSet({ quality: { successRate: { value: 0.6, mode: "live", unit: "ratio", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true, JSON.stringify(r.failures))
  })

  test("live quality drift over absAbs 0.25 default → FAIL", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    // successRate 0.8 → 0.4, |Δ|=0.4 > 0.25 default
    const current = makeSet({ quality: { successRate: { value: 0.4, mode: "live", unit: "ratio", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, false)
  })

  test("mechanism layer uses 5% default (tighter than cost)", () => {
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(makeSet()), captured: "x", metricSet: makeSet() }
    // avgToolsExposed 28.62 → 30.5 → +6.6% > 5% mechanism default
    const current = makeSet({ mechanism: { avgToolsExposed: { value: 30.5, mode: "live", unit: "count", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, false)
    assert.ok(r.failures.find(f => f.key === "avgToolsExposed"))
  })
})

// ── replay-mode strict default ──────────────────────────────────────────────

describe("checkGolden — replay mode strict", () => {
  test("replay mode: any non-trivial Δ fails", () => {
    const baseMeta = { mode: "replay" }
    const goldenSet = makeSet({ meta: baseMeta })
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    // inputTokens 1000 → 1001, only 0.1% but replay should still fail
    const current = makeSet({ meta: baseMeta, cost: { inputTokens: { value: 1001, mode: "replay", unit: "tokens", samples: 1 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, false, "replay should be strict")
  })

  test("replay mode: exact match passes", () => {
    const baseMeta = { mode: "replay" }
    const goldenSet = makeSet({ meta: baseMeta })
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    const r = checkGolden(makeSet({ meta: baseMeta }), golden)
    assert.equal(r.passed, true)
  })
})

// ── per-metric override beats per-layer default ────────────────────────────

describe("checkGolden — tolerance precedence", () => {
  test("metric-level override beats layer default", () => {
    const goldenSet = makeSet()
    const golden = {
      schema: "deepstrike-bench-golden/v0",
      key: keyFromMetricSet(goldenSet),
      captured: "x",
      metricSet: goldenSet,
      tolerance: { metrics: { "cost.inputTokens": { absPct: 1 } } }, // 1% — tighter than 10%
    }
    // +5% would normally pass under cost.absPct=10, but the metric override caps at 1%
    const current = makeSet({ cost: { inputTokens: { value: 1050, mode: "live", unit: "tokens", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, false)
    const failure = r.failures.find(f => f.key === "inputTokens")
    assert.equal(failure.tolerance.absPct, 1)
  })

  test("layer-level override beats mode default", () => {
    const goldenSet = makeSet()
    const golden = {
      schema: "deepstrike-bench-golden/v0",
      key: keyFromMetricSet(goldenSet),
      captured: "x",
      metricSet: goldenSet,
      tolerance: { layers: { cost: { absPct: 50 } } }, // very loose
    }
    // +30% normally fails cost.absPct=10, but the layer override allows up to 50%
    const current = makeSet({ cost: { inputTokens: { value: 1300, mode: "live", unit: "tokens", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true, JSON.stringify(r.failures))
  })
})

// ── missing + extra metrics ────────────────────────────────────────────────

describe("checkGolden — missing + extras", () => {
  test("missing metric in current → FAIL with reason=missing", () => {
    const goldenSet = makeSet()
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    const current = makeSet()
    delete current.cost.inputTokens
    const r = checkGolden(current, golden)
    const failure = r.failures.find(f => f.key === "inputTokens")
    assert.equal(failure.reason, "missing")
    assert.equal(failure.current, null)
  })

  test("extra metric in current → listed in extras, not a failure", () => {
    const goldenSet = makeSet()
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    const current = makeSet()
    current.cost.newMetric = { value: 42, mode: "live", unit: "count", samples: 4 }
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true)
    assert.ok(r.extras.find(e => e.key === "newMetric"))
  })
})

// ── zero-golden edge case ──────────────────────────────────────────────────

describe("checkGolden — zero golden value edge", () => {
  test("golden=0, current=0 → PASS (no false alarm)", () => {
    const goldenSet = makeSet({ cost: { tokensPerTurn: { value: 0, mode: "live", unit: "tokens", samples: 4, stdev: 0 } } })
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    const current = makeSet({ cost: { tokensPerTurn: { value: 0, mode: "live", unit: "tokens", samples: 4, stdev: 0 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true)
  })

  test("golden=0, current=tiny → still PASS under abs floor", () => {
    const goldenSet = makeSet({ quality: { successRate: { value: 0, mode: "live", unit: "ratio", samples: 4 } } })
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    // absAbs default for quality is 0.25 — 0.05 is well under
    const current = makeSet({ quality: { successRate: { value: 0.05, mode: "live", unit: "ratio", samples: 4 } } })
    const r = checkGolden(current, golden)
    assert.equal(r.passed, true, JSON.stringify(r.failures))
  })
})

// ── render smoke ────────────────────────────────────────────────────────────

describe("renderGoldenCheck", () => {
  test("renders PASS / FAIL / extras", () => {
    const goldenSet = makeSet()
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "2026-06-16T00:00:00Z", metricSet: goldenSet }

    // current with one failure and one extra
    const current = makeSet({ cost: { inputTokens: { value: 1500, mode: "live", unit: "tokens", samples: 4 } } })
    current.mechanism.brandNew = { value: 7, mode: "live", unit: "count", samples: 4 }
    const r = checkGolden(current, golden)

    const out = renderGoldenCheck(r)
    assert.match(out, /golden check/)
    assert.match(out, /FAIL/)
    assert.match(out, /cost\.inputTokens/)
    assert.match(out, /new metric/)
    assert.match(out, /mechanism\.brandNew/)
  })

  test("renders PASS when no failures", () => {
    const goldenSet = makeSet()
    const golden = { schema: "deepstrike-bench-golden/v0", key: keyFromMetricSet(goldenSet), captured: "x", metricSet: goldenSet }
    const r = checkGolden(makeSet(), golden)
    const out = renderGoldenCheck(r)
    assert.match(out, /PASS/)
  })
})
