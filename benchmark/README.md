# `benchmark/` — mechanism evaluation harness

A scenario × variant runner that produces a unified metric set and Δ comparison for every mechanism
in the kernel (tool gating, prefix-cache, compression, orchestration, signal preemption, governance,
token tiering). Spec: [`.local-docs/specs/benchmark-harness.md`](../.local-docs/specs/benchmark-harness.md).

This is the **independent benchmark tree**. It deliberately lives outside `node/` so it can grow
into a 4-SDK consumer without re-housing later.

## Status — PR #1 (BM0a + BM2 minimal Δ pipeline)

Done in this PR:

- `core/metrics.mjs` — `MetricSet` schema (5 layers: cost / latency / quality / contextHealth / mechanism), per-metric `mode` + `samples` + `stdev`
- `core/diff.mjs` — pure baseline-vs-variant Δ with stdev-aware significance
- `core/render.mjs` — fixed-width table renderer
- `adapters/dwell-report.mjs` — legacy `dwell-report.json` → `MetricSet`
- `pricing/pricing.json` — hand-maintained provider×model price list
- `cli/diff.mjs` — single CLI: read two runs (MetricSet **or** dwell-report) → Δ table

Deferred to later PRs (see spec §8):

- BM0b — runner integration with `RuntimeOptions.onTurnMetrics` + latency timing
- BM1 — variant runner + `bench` CLI (`--variants`, `--samples`)
- BM1.1 — `--mode replay` for deterministic re-runs
- BM3 — `gen_eval` LLM-judge integration for quality
- BM4 — `baselines/*.json` regression gate (CI)
- BM5 — scenario library coverage (§7 matrix)

## Quick start — diff two existing dwell runs

```bash
# 1. Run the dwell example twice (it already writes dwell-report.json):
cd ../node && npm run build
node examples/tool-gating-dwell.mjs --tasks 4 --max-turns 12              # → run-A/dwell-report.json
node examples/tool-gating-dwell.mjs --tasks 4 --max-turns 12 --gate       # → run-B/dwell-report.json

# 2. Diff them:
cd ../benchmark
node cli/diff.mjs \
  ../node/examples/.gating-runs/run-<A-stamp>/dwell-report.json \
  ../node/examples/.gating-runs/run-<B-stamp>/dwell-report.json \
  --baseline-id off --variant-id on --scenario gating-dwell
```

Output (illustrative):

```
══════════════════════════════════════════════════════════════════════════
  scenario gating-dwell  ·  baseline=off(live, n=4)  ·  variant=on(live, n=4)
══════════════════════════════════════════════════════════════════════════
  metric                                  baseline          variant            Δ           Δ%       sig
  ──────────────────────────────────────────────────────────────────────────────────────────────────
  [cost]
  cacheHitRate                            0.77              0.63               -0.14       -18.2%   —
  cacheReadTokens                         48,920            38,640             -10,280     -21%     —
  dollars                                 0.0123            0.0061             -0.0062     -50.4%   —
  inputTokens                             62,310            22,180             -40,130     -64.4%   —
  tokensPerTurn                           2,810±150         980±90             -1,830      -65.1%   ✓
  [mechanism]
  avgToolsCalled                          0.92              0.95               0.03        +3.3%    —
  avgToolsExposed                         29.4±0.8          7.0±0              -22.4       -76.2%   ✓
  dwellMean                               5.2               5.4                0.2         +3.8%    —
  ...
══════════════════════════════════════════════════════════════════════════
```

`sig ✓` = `|Δ| > 2σ` (live with stdev on both sides). `—` = cannot decide (modes/stdev mismatch);
caller judges by Δ%.

## Files

```
benchmark/
├── core/
│   ├── metrics.mjs         MetricSet schema + JSDoc types + assertMetricSet
│   ├── diff.mjs            baseline vs variant Δ (pure)
│   └── render.mjs          fixed-width table renderer
├── adapters/
│   └── dwell-report.mjs    legacy dwell-report.json → MetricSet
├── pricing/
│   └── pricing.json        provider:model → $/M tokens (hand-maintained)
├── cli/
│   └── diff.mjs            CLI entry
└── README.md               this file
```

## Design rules (enforced by structure)

1. **Replay measures cost, live measures quality.** A mechanism that changes the prompt (gating
   narrows `tools`, compression rewrites history) will look like a free win in replay — replay is
   the *floor* on cost savings, not proof the model still completes the task. Always pair with a
   live run when judging an A/B.
2. **Same-provider comparisons only.** Cache accounting differs across providers (Anthropic reports
   read + creation, DeepSeek reports only read, OpenAI proxies often report 0). Diffing across
   providers is meaningful for Δ% only — absolutes are not comparable.
3. **Pricing missing ⇒ `$` is `n/a`, not blocking.** The diff table omits `dollars` rather than
   computing a bogus value when `pricing/pricing.json` lacks the `provider:model` key.
4. **No SDK imports yet.** This PR's adapter reads `dwell-report.json` as plain data — no SDK type
   surface. BM0b introduces a typed boundary at the runner integration point.
