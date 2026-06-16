# `benchmark/` — mechanism evaluation harness

A scenario × variant runner that produces a unified metric set and Δ comparison for every mechanism
in the kernel (tool gating, prefix-cache, compression, orchestration, signal preemption, governance,
token tiering). Spec: [`.local-docs/specs/benchmark-harness.md`](../.local-docs/specs/benchmark-harness.md).

This is the **independent benchmark tree**. It deliberately lives outside `node/` so it can grow
into a 4-SDK consumer without re-housing later.

## Status

**Done:**

- **PR #1 — BM0a + BM2** (minimal Δ pipeline):
  - `core/metrics.mjs` — `MetricSet` schema (5 layers: cost / latency / quality / contextHealth / mechanism), per-metric `mode` + `samples` + `stdev`
  - `core/diff.mjs` — pure baseline-vs-variant Δ with stdev-aware significance
  - `core/render.mjs` — fixed-width table renderer
  - `adapters/dwell-report.mjs` — legacy `dwell-report.json` → `MetricSet`
  - `pricing/pricing.json` — hand-maintained provider×model price list
  - `cli/diff.mjs` — read two runs (MetricSet **or** dwell-report) → Δ table

- **PR #2 — BM1 + BM0b lite** (variant runner + CLI + first scenario):
  - `core/scenario.mjs` — `BenchScenario` / `BenchVariant` (JSDoc types)
  - `core/runner.mjs` — `runBench(scenario, variantId, opts)` driving the SDK runtime under live mode, capturing `onTurnMetrics`
  - `core/aggregator.mjs` — session records → MetricSet with per-session mean+stdev
  - `scenarios/gating-dwell.mjs` — first scenario: 4 dev tasks × ~30 tools × 4 skills, variants `off` / `on`
  - `scenarios/index.mjs` — scenario registry
  - `cli/bench.mjs` — `bench <scenario> [--variants ...] [--compare]`
  - `utils/{env,sdk}.mjs` — env loading + SDK dynamic import + provider resolution

- **PR #3 — BM1.1** (deterministic replay via SDK-level `ReplayProvider`):
  - Node SDK gains `ReplayProvider` + `extractRecordedMessages` (lives at `node/src/runtime/replay-{provider,fixture}.ts`, exported from `node/src/index.ts`). 15-test determinism suite.
  - `core/runner.mjs` + `cli/bench.mjs` gain `--mode replay --fixture <run-dir> [--fixture-from <variant>]`. Replay reads each task's prior `events.json`, returns recorded LLM responses, never hits an API.
  - `MetricSet.meta.mode = "replay"` ⇒ diff renderer marks any non-zero Δ as significant (deterministic, no sample noise).

- **PR #4 — BM3** (LLM-judge quality scoring via SDK `judge()`):
  - Node SDK gains `judge({ provider, goal, criteria, result })` wrapping the kernel's `gen_eval` free functions (`buildEvalMessages` / `parseVerdict` / `verdictOutputSchema`). 9-test suite. Exports include `Criterion`, `Verdict` types.
  - `core/runner.mjs` calls `judge()` after each session when enabled; max_turns / error runs get a structured incomplete-marker so the judge grades them honestly instead of pass-by-omission.
  - `core/aggregator.mjs` emits `quality.successRate` (pass ratio) + `quality.overallScore` (mean 0..1).
  - `cli/bench.mjs` gains `--judge` / `--no-judge` (default: ON in live, OFF in replay) + `--judge-provider <id>` + `--judge-model <model>`.

- **PR #5 — BM4** (golden baselines + regression gate):
  - `core/golden.mjs` — golden save / load / check + tolerance policy. Layer defaults: replay strict (0.001%); live cost 10% / quality 0.25 abs / mechanism 5% / contextHealth 10% / latency 50%. Per-metric overrides win over per-layer; per-layer wins over mode default.
  - `tests/golden.test.mjs` — 20-test suite under Node's built-in `--test` runner (zero new deps).
  - `core/aggregator.mjs` — under replay mode, skip wall-clock latency metrics (process overhead, not mechanism cost; surfaces as false-positive significance under strict tolerance). Closes BM1.1 backlog #9.
  - `cli/bench.mjs` — `--baseline-save` / `--baseline-check` / `--baseline-update` / `--baseline-dir <dir>`. `--baseline-check` exits 2 on any failure → CI-gateable.

- **PR #6 — BM5: compression-stress scenario** (first BM5 mechanism coverage):
  - `scenarios/compression-stress.mjs` — long-loop task (review 12 PRs sequentially, then summarize). Variants `budget-loose` (maxTokens=8192) vs `budget-tight` (maxTokens=2048) force different compression regimes. mechanismHook emits per-compression-action counts + `completionRatio` (prCallCount / 12) + `summarizeCallCount`.
  - Verified live on DeepSeek: `prCallCount −66.7%`, `completionRatio −67%`, `compressions +100%`, `inputTokens −89.3%`, `dollars −87.6%` — tight budget saves 88% of cost but completes only 33% of the task. First framework-quantified cost/quality trade-off (spec §6.1's warning made measurable).

- **PR #7 — Judge tool-arg fix + governance scenario + `--samples N`** (BM3 backlog #24 + BM5 #2 + BM1.2):
  - `core/runner.mjs` `buildJudgeResult` now folds every tool-call's name + truncated arguments (cap 1500 chars per arg) into the judge prompt — scenarios whose deliverable rides in a tool argument (e.g. `summarize_findings(summary)`) are no longer invisible to the judge. Closes BM3 backlog #24; compression-stress's `budget-loose` rerun went from judge score 0 → 0.5.
  - `scenarios/governance-write-deny.mjs` — second BM5 scenario: "diagnose + fix the failing auth test"; variants `unrestricted` vs `write-denied` (kernel `governancePolicy.rules` denies `write_file` + `run_bash`). mechanismHook tracks executed-tool counts + `rollbacks` (the kernel's denial signal — the model's call was intercepted and the turn rolled back). Verified: quality preserved at 100% in both variants (graceful degradation), but write-denied costs +42% wallMs + 2 rollbacks + 1 extra turn.
  - `--samples N` flag (default 1) repeats the full task list N times per variant; the aggregator pools sessions × samples so stdev tightens. Verified with `--samples 2` on the governance scenario.
- BM3 — `gen_eval` LLM-judge for quality (`successRate` is the empty slot in `quality{}` today)
- BM4 — `baselines/*.json` regression gate (CI)
- BM5 — scenario coverage (§7): prefix-cache, compression, memory, orchestration, signals, governance, token-tiering

## Quick start — `bench` runner (PR #2 path)

```bash
# 1. Build the Node SDK once (runner loads dist):
cd ../node && npm run build

# 2. Run gating-dwell across both variants on DeepSeek, and diff:
cd ../benchmark
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --compare
```

Output structure:

```
benchmark/.runs/gating-dwell-<stamp>/
├── gating-dwell.off/
│   ├── metricset.json            ← the off-variant MetricSet
│   ├── debug.events.json         ← raw session events per task (for debugging)
│   ├── review.events.json
│   ├── test.events.json
│   └── refactor.events.json
├── gating-dwell.on/
│   ├── metricset.json
│   └── ...events
└── diff.off-vs-on.json           ← saved Δ result (when --compare)
```

The `--compare` flag prints the Δ table to stdout (or `--json` to emit a structured DiffResult).

### Other examples

```bash
# List available scenarios:
node cli/bench.mjs list

# Run only one variant, only 2 tasks (smoke):
node cli/bench.mjs gating-dwell --variants on --tasks 2 --provider openai

# Custom output dir:
node cli/bench.mjs gating-dwell --variants off,on --output /tmp/bench-test --compare
```

## Replay mode — deterministic re-runs from a prior fixture (PR #3 path)

Replay reads back the `events.json` files a prior live run wrote (no API call,
`$0`, ~milliseconds per task) and feeds them through the SDK's `ReplayProvider`.
Two use modes:

**Sanity replay** — each variant replays its own prior fixture. Use it to detect
runner regressions and to re-cost an old run under updated pricing:

```bash
# Pre-req: a prior live run at benchmark/.runs/<stamp>
node cli/bench.mjs gating-dwell --variants off,on --mode replay \
  --fixture .runs/gating-dwell-<stamp> --compare
```

**Cross-variant pin** — every variant replays the SAME variant's fixture
(`--fixture-from`). Model behavior is held constant; the only thing that
differs between off and on is the variant's `RuntimeOptions` overlay (skill
files, `stableCoreToolIds`, …). The resulting Δ is **purely the prompt-size
cost of the mechanism**, free of LLM noise:

```bash
node cli/bench.mjs gating-dwell --variants off,on --mode replay \
  --fixture .runs/gating-dwell-<stamp> --fixture-from off --compare
```

Caveat: under replay the cache layer is not modeled (`cacheReadTokens = 0`,
`cacheHitRate = 0`). Replay measures the cost floor under no-cache; for the
cache-bust tension a mechanism may introduce, you still need a live A/B.

## Quality scoring with `--judge` (PR #4 path)

In live mode the bench runner calls the SDK's `judge()` over each session's
final output, populating `quality.successRate` (pass ratio across sessions) and
`quality.overallScore` (mean 0..1). Default: ON in live, OFF in replay
(replayed responses are deterministic across variants — judge would produce
identical verdicts).

```bash
# Default — judge is on:
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --compare

# Use a cheaper model for the judge to keep cost down:
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek \
  --judge-provider openai --judge-model gpt-4o-mini --compare

# Skip judge (cost-only A/B):
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --no-judge --compare
```

Each session contributes one judge call. For incomplete runs (max_turns /
error) the judge sees a structured `AGENT_INCOMPLETE (status=…): ran N turns,
M tool-call rounds. Last assistant text: …` so it can grade partial work
instead of pass-by-omission.

## Golden baselines — regression gate (PR #5 path)

Goldens freeze a MetricSet as the expected shape; subsequent runs check
against them with a tolerance policy and exit non-zero on any failure. The CI
contract: `bench … --baseline-check` exits 2 if any metric drifts past its
tolerance — a merge can be gated on that.

```bash
# First time: record one golden per (scenario, variant, provider, model, mode):
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --baseline-save

# Then in CI:
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --baseline-check
echo "exit $?"   # 0 = all goldens met, 2 = at least one failure

# Refresh golden when an intentional change moves a metric:
node cli/bench.mjs gating-dwell --variants off,on --provider deepseek --baseline-update
```

Golden files live at `benchmark/baselines/<scenarioId>/<variantId>.<provider>.<model>.<mode>.json`
and embed the full reference MetricSet plus an optional tolerance policy:

```json
{
  "schema": "deepstrike-bench-golden/v0",
  "key": { "scenarioId": "gating-dwell", "variantId": "off",
           "provider": "deepseek", "model": "deepseek-chat", "mode": "live" },
  "captured": "2026-06-16T…",
  "tolerance": {
    "layers": { "cost": { "absPct": 5 } },         /* tighter than the default 10% */
    "metrics": { "cost.cacheHitRate": { "absAbs": 0.05 } }
  },
  "metricSet": { /* … the frozen MetricSet … */ }
}
```

**Tolerance defaults** (per layer, applied when no override is set):

| layer         | replay mode    | live mode             |
| ------------- | -------------- | --------------------- |
| cost          | strict 0.001%  | 10%                   |
| latency       | strict 0.001%  | 50% (machine-dep.)    |
| quality       | strict 0.001%  | abs 0.25 (judge noisy)|
| contextHealth | strict 0.001%  | 10%                   |
| mechanism     | strict 0.001%  | 5%                    |

Override lookup order: per-metric `tolerance.metrics["{layer}.{key}"]` →
per-layer `tolerance.layers[layer]` → mode default. Human-edited tolerances
survive `--baseline-save` (they're inherited when the saved golden carries no
explicit policy).

**Replay-mode goldens are the cheapest CI gate.** A replay run is
deterministic + free, so the strict 0.001% tolerance catches any unintended
behavior change in the runner / aggregator / scenario without the noise of a
live LLM. Replay goldens depend on the fixture run that recorded them, so
keep the fixture alongside the golden (or commit both).

## Diff already-recorded runs (PR #1 path, still supported)

```bash
# Diff two MetricSet JSONs:
node cli/diff.mjs runs/off.json runs/on.json

# Or two legacy dwell-report.json files (auto-imported):
node cli/diff.mjs \
  ../node/examples/.gating-runs/run-<A>/dwell-report.json \
  ../node/examples/.gating-runs/run-<B>/dwell-report.json \
  --baseline-id off --variant-id on --scenario gating-dwell
```

Output:

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
│   ├── scenario.mjs        BenchScenario / BenchVariant JSDoc types
│   ├── runner.mjs          runBench: scenario × variant → live MetricSet
│   ├── aggregator.mjs      session records → MetricSet w/ per-session stdev
│   ├── diff.mjs            baseline vs variant Δ (pure)
│   └── render.mjs          fixed-width table renderer
├── scenarios/
│   ├── gating-dwell.mjs    first scenario (gating off vs on)
│   └── index.mjs           scenario registry
├── adapters/
│   └── dwell-report.mjs    legacy dwell-report.json → MetricSet
├── pricing/
│   └── pricing.json        provider:model → $/M tokens (hand-maintained)
├── utils/
│   ├── env.mjs             .env loader (matches existing scripts)
│   └── sdk.mjs             loadSdk + resolveProvider
├── cli/
│   ├── bench.mjs           runner CLI
│   └── diff.mjs            standalone diff CLI
└── README.md               this file
```

## Scenarios

| id                      | mechanism | variants                        | notes |
| ----------------------- | --------- | ------------------------------- | ----- |
| `gating-dwell`          | tool gating + skill A/B | `off` / `on` | 4 dev tasks × ~30 tools × 4 skills; reproduces the original dwell A/B finding |
| `compression-stress`    | context compression budget | `budget-loose` / `budget-tight` | 12-PR sequential review; surfaces compression's task-completion cost |
| `governance-write-deny` | kernel governance policy | `unrestricted` / `write-denied` | fix-failing-test; `write_file` + `run_bash` denied → measures graceful degradation + rollback overhead |
| `memory-recall`         | long-term memory (DreamStore) | `memory-empty` / `memory-preloaded` | diagnose-outage; pre-seeded memory cuts turns ~57% / cost ~55% at preserved quality |
| `signal-injection`      | RuntimeSignal urgency | `no-signal` / `soft-interrupt` / `hard-interrupt` | counter-based `SignalSource` injects on turn 4; soft-interrupt completes the loop with the [SIGNAL] note acknowledged, hard-interrupt is curtailed to ~3 turns |

`bench list` prints the same data at runtime.

### §7 mechanism coverage matrix

The harness spec ([`benchmark-harness.md` §7](../.local-docs/specs/benchmark-harness.md))
calls out 8 kernel mechanisms that should each get an A/B scenario. Current state:

| § | mechanism | scenario | status | notes |
| - | --------- | -------- | ------ | ----- |
| 7.1 | tool gating | `gating-dwell` | ✅ shipped | full A/B with cache-bust tension surfaced |
| 7.2 | prefix-cache / attention | — | 🚫 blocked | SDK currently hard-codes Anthropic breakpoint placement; need a `cacheBreakpointStrategy` option before an A/B is meaningful |
| 7.3 | context compression / paging | `compression-stress` | ✅ shipped | reveals task-completion cost of tight budget |
| 7.4 | memory / knowledge | `memory-recall` | ✅ shipped | pre-seeded `DreamStore` vs. empty; carries a scenario-local `InMemoryDreamStore` (TODO: promote to public SDK export) |
| 7.5 | orchestration / sub-agents | — | ⏸ deferred | DAG vs. serial workflow on the same task; ~300 LOC, no SDK blocker |
| 7.6 | signal preemption | `signal-injection` | ✅ shipped | soft `Interrupt` (High) vs. `InterruptNow` (Critical) A/B; soft path keeps run going (12/12 fetches), hard path preempts at the inject turn |
| 7.7 | governance gate | `governance-write-deny` | ✅ shipped | rollbacked-event signal documented in scenario header |
| 7.8 | token-count tiering | — | 🚫 blocked | kernel only ships `CharApproxCounter`; `SetTokenizer` event handler is a no-op, so a `--tokenizer tiktoken` variant wouldn't change behavior. Belongs to the [kernel optimization #5](../.local-docs/specs/agent-os-status-2026-06.md) backlog |

Legend: ✅ shipped · ⏸ deferred (no blocker, just unscheduled) · 🚫 blocked (needs SDK or kernel work first).

## Adding a scenario

A `BenchScenario` is a strategy object exposing `tasks`, `mkTools`, `systemPrompt`, and one
`BenchVariant` per knob position. The variant's `setup` hook returns a `runtimeOverlay` merged into
`RuntimeOptions` and an optional `cleanup` — that's where mechanism-specific config lands (skill
files, `stableCoreToolIds`, compaction policy, `extensions`, etc.). See
[`scenarios/gating-dwell.mjs`](scenarios/gating-dwell.mjs) for the gating pattern and
[`scenarios/compression-stress.mjs`](scenarios/compression-stress.mjs) for a single-task long-loop
pattern. Register the scenario in [`scenarios/index.mjs`](scenarios/index.mjs).

Mechanism-specific metrics ride in `BenchScenario.mechanismHook({ events, turnMetrics })` →
`Record<string, number>`. The aggregator turns the per-session outputs into the `mechanism` layer of
the resulting MetricSet with mean + stdev across sessions.

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
4. **No SDK imports yet.** PR #1's adapter reads `dwell-report.json` as plain data — no SDK type
   surface. BM0b will introduce a typed boundary at the runner integration point.
