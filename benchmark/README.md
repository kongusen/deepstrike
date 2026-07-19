# `benchmark/` — mechanism evaluation harness

A scenario × variant runner that produces a unified metric set and Δ comparison for every mechanism
in the kernel (tool gating, prefix-cache, compression, orchestration, signal preemption, governance,
token tiering). Spec: [`.local-docs/specs/benchmark-harness.md`](../.local-docs/specs/benchmark-harness.md).

This is the **independent benchmark tree**. It deliberately lives outside `node/` so it can grow
into a 4-SDK consumer without re-housing later.

> **Capability evals (BFCL / GAIA / WebArena):** task-completion suites live under
> [`capability/`](capability/README.md) and are separate from mechanism A/B scenarios.
> Quick start: `node capability/cli/capability.mjs bfcl --provider deepseek --limit 8`

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
  - `scenarios/governance-write-deny.mjs` — second BM5 scenario: "diagnose + fix the failing auth test". The original v0.2.22 run used rollback-mode denial and found +42% wallMs, 2 rollbacks, and 1 extra turn; current kernels commit a visible denied result and retain `rollbacks` only as a zero-regression metric.
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

# Orchestration F1–F3 (stub driver — no API key):
node cli/bench.mjs orchestration-f1 --variants weighted,fifo --compare
node cli/bench.mjs orchestration-f2 --variants weighted,fifo --compare
node cli/bench.mjs orchestration-f3 --variants weighted,fifo --compare
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

## Self-harness lab — `selfharness/`

An offline propose–validate–promote loop after *Self-Harness: Harnesses That Improve Themselves*
(arXiv:2606.09498): a fixed model mines its own verifier-anchored failure clusters, proposes bounded
JSON patches against the declared editable surfaces of a `HarnessManifest` (SDK
`@deepstrike/sdk/harness`), and only edits that pass the paper's conservative rule
(`Δ_in ≥ 0 ∧ Δ_ho ≥ 0 ∧ max > 0`) on a held-in/held-out split are promoted into the lineage.
Held-out isolation is structural: only held-in evidence ever reaches the proposer.
Spec: [`.local-docs/specs/self-harness-loop.md`](../.local-docs/specs/self-harness-loop.md).

```
node benchmark/selfharness/cli.mjs \
     --adapter ./benchmark/selfharness/adapters/format-discipline.mjs \
     --held-in json-strict,word-limit,checklist --held-out csv-strict,summary-limit \
     --rounds 2 --k 3 --repeats 2 --provider deepseek
```

- `evidence.mjs` / `trace-excerpt.mjs` — deterministic failure-signature clustering + bounded
  Fig-7-style excerpts from `*.events.json` streams (LLM-free)
- `miner.mjs` / `proposer.mjs` — the two LLM slots (`complete(prompt)` injected; canned in tests)
- `validate.mjs` / `loop.mjs` — acceptance rule, disjoint-surface merge, `.harness-lab/` lineage
  (`<digest>.json` per manifest + `rounds.jsonl` per round; no timestamps — byte-stable records)
- `adapters/` — `fixture` (deterministic, zero-cost, CI), `live` (real runs + `judge()`),
  `format-discipline` (small live task set exercising strict-output-format failure mechanisms)

Statistical honesty: a handful of tasks × `--repeats 1` cannot adjudicate fine-grained deltas (a
lucky +1 will get promoted and evaporate). Production per-model profile runs need a real task
corpus and `--repeats ≥ 2` — the paper used 64 tasks × 2 repeats.

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
├── selfharness/
│   ├── evidence.mjs        failure records → deterministic signature clusters → EvidenceBundle
│   ├── trace-excerpt.mjs   bounded Fig-7-style trajectory rendering
│   ├── miner.mjs           mechanism attribution (LLM slot, addressability filter)
│   ├── proposer.mjs        ≤K bounded HarnessPatch proposals (LLM slot)
│   ├── validate.mjs        evaluate + acceptance rule + disjoint merge
│   ├── loop.mjs            propose→validate→promote ring + lineage store
│   ├── cli.mjs             shell driver
│   └── adapters/           fixture (CI) · live (real runs) · format-discipline (live task set)
├── utils/
│   ├── env.mjs             .env loader (matches existing scripts)
│   └── sdk.mjs             loadSdk + resolveProvider
├── cli/
│   ├── bench.mjs           runner CLI
│   └── diff.mjs            standalone diff CLI
└── README.md               this file
```

## Scenarios

Each scenario is a single-variable A/B (variants differ **only** by `RuntimeOptions` overlay) so
the metric Δ isolates one mechanism's contribution. `bench list` prints the same data at runtime.

| id                      | mechanism | variants                        | headline finding |
| ----------------------- | --------- | ------------------------------- | ---------------- |
| `gating-dwell`          | tool gating + skill A/B | `off` / `on` | 30-tool surface → `avgToolsExposed` **−66.6%**, `tokensPerTurn` **−32.8%**; `cacheHitRate` −9.3% (epoch-switch cache-bust tension surfaced) |
| `compression-stress`    | context compression budget | `budget-loose` / `budget-tight` | tight saves **88%** dollars but only completes **33%** of the 12-PR review — first framework-quantified cost/quality trade-off |
| `governance-write-deny` | kernel governance policy | `unrestricted` / `write-denied` / `write-denied-pre-filtered` | attempted denials commit visible error results; `rollbacks` must remain 0; pre-filtering prevents known-denied attempts entirely |
| `memory-recall`         | long-term memory (DreamStore) | `memory-empty` / `memory-preloaded` | pre-seeded memory cuts `turnsToDone` **−57%**, `wallMs` −47%, `inputTokens` −65%, `dollars` −55% at preserved quality |
| `signal-injection`      | RuntimeSignal urgency | `no-signal` / `soft-interrupt` / `hard-interrupt` | soft (High) injects `[SIGNAL]` and completes 12/12; hard (Critical) preempts in-flight LLM call within ~1 turn of inject |
| `prefix-cache`          | Anthropic `cache_control` strategy | `default` / `tools-only` / `system-only` / `frozen-prefix` / `none` | DeepSeek smoke verified plumbing (no-op on auto-cache providers as designed); **Anthropic A/B verify deferred** until an `ANTHROPIC_API_KEY` is wired up — strategy delta is observable above the 1024-token cacheable-block threshold |
| `orchestration-f1`      | DAG scheduler critical-path | `weighted` / `fifo` | stub `runWorkflow` (no LLM): weighted starts chain in wave 0 (`firstHeadIsChain=1`); fifo delays to wave 2 |
| `orchestration-f2`      | DAG scheduler loop fairness | `weighted` / `fifo` | concurrency=1: `independentWaitWaves=1` under both policies (re-arm yields to peer) |
| `orchestration-f3`      | DAG failure propagation | `weighted` / `fifo` | upstream fail → `failedNodes=1`, `skippedUpstreamNodes=2` (transitive close) |

### §7 mechanism coverage matrix

The harness spec ([`benchmark-harness.md` §7](../.local-docs/specs/benchmark-harness.md))
calls out 8 kernel mechanisms that should each get an A/B scenario. Current state — **7 shipped,
1 deferred** (designs preserved in [`.local-docs/specs/bench-scenarios-deferred.md`](../.local-docs/specs/bench-scenarios-deferred.md)):

| § | mechanism | scenario | status | notes |
| - | --------- | -------- | ------ | ----- |
| 7.1 | tool gating | `gating-dwell` | ✅ shipped | full A/B with cache-bust tension surfaced |
| 7.2 | prefix-cache / attention | `prefix-cache` | ✅ shipped | unblocked in v0.2.22 by the `cacheBreakpointStrategy` SDK knob; 5-variant A/B over `default` / `tools-only` / `system-only` / `frozen-prefix` / `none`; Anthropic-only signal (non-Anthropic providers ignore the extension by design) |
| 7.3 | context compression / paging | `compression-stress` | ✅ shipped | reveals task-completion cost of tight budget |
| 7.4 | memory / aspiration | `memory-recall` | ✅ shipped | pre-seeded `DreamStore` vs. empty; uses the public `InMemoryDreamStore` (promoted to SDK in v0.2.21) |
| 7.5 | orchestration / sub-agents | `orchestration-f1` / `orchestration-f2` / `orchestration-f3` | ✅ shipped | `scheduler_policy` A/B (`weighted` vs `fifo`); stub `runWorkflow` driver (no LLM). F1 critical-path makespan, F2 loop fairness, F3 failure→`skipped_upstream_failed`. Kernel twins: `orchestration::workflow::run::tests::{f1,f2,f3}_*` |
| 7.6 | signal preemption | `signal-injection` | ✅ shipped | soft `Interrupt` (High) vs. `InterruptNow` (Critical) A/B; soft path keeps run going (12/12 fetches), hard path preempts at the inject turn |
| 7.7 | governance gate | `governance-write-deny` | ✅ shipped | visible denial results + zero-rollback regression signal |
| 7.8 | token-count tiering | — | ⏸ deferred | no natural variant dimension that doesn't reduce to tokenizer-accuracy noise rather than run-behavior signal; framework's A/B pattern is a poor fit. Design preserved in the deferred-scenarios doc; kernel `SetTokenizer` work also pending |

Legend: ✅ shipped · ⏸ deferred (design preserved, scenario not built).

## Adding a scenario

A `BenchScenario` is a strategy object exposing `tasks`, `mkTools`, `systemPrompt`, and one
`BenchVariant` per knob position. The variant's `setup` hook returns a `runtimeOverlay` merged into
`RuntimeOptions` and an optional `cleanup` — that's where mechanism-specific config lands (skill
files, `stableCoreToolIds`, compaction policy, `extensions`, etc.). See
[`scenarios/gating-dwell.mjs`](scenarios/gating-dwell.mjs) for the gating pattern and
[`scenarios/compression-stress.mjs`](scenarios/compression-stress.mjs) for a single-task long-loop
pattern. Kernel-deterministic workflow benches use optional `driveTask` + `requiresProvider: false`
(see [`scenarios/orchestration-scheduler.mjs`](scenarios/orchestration-scheduler.mjs)). Register the
scenario in [`scenarios/index.mjs`](scenarios/index.mjs).

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
