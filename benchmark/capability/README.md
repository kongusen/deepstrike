# Capability eval (BFCL / GAIA / WebArena)

External **task-completion** benchmarks for DeepStrike agents — separate from the mechanism A/B harness in [`../README.md`](../README.md).

| Layer | What it measures |
|-------|------------------|
| `benchmark/scenarios/*` + `cli/bench.mjs` | Kernel mechanism Δ (gating, compression, governance, …) |
| `benchmark/capability/` (this tree) | Can the agent complete BFCL / GAIA / WebArena-style tasks? |

Shared: Node SDK load + provider resolution via [`../utils/sdk.mjs`](../utils/sdk.mjs). Default provider: **DeepSeek** (`DEEPSEEK_API_KEY` in repo `.env`).

## Quick start

```bash
# 1. Build Node SDK
cd node && npm run build && cd ../benchmark

# 2. List suites
node capability/cli/capability.mjs list

# 3. BFCL smoke (tool-call accuracy, deterministic grader)
node capability/cli/capability.mjs bfcl --provider deepseek --limit 8

# 4. GAIA smoke (tools + normalized final answer)
node capability/cli/capability.mjs gaia --provider deepseek --limit 5
```

Outputs land in `benchmark/.runs/capability-<suite>-<stamp>/`:

- `report.json` — `CapReport` (`accuracy`, `meanScore`, per-task `grade`)
- `<taskId>.result.json` — text, tool calls, grade
- `<taskId>.events.json` — session events for debugging

## Suites

| id | Status | Scoring |
|----|--------|---------|
| `bfcl` | Smoke (10 built-in tasks) | Exact tool name + normalized args (no LLM-judge) |
| `gaia` | Smoke (5 built-in tasks) | `Final answer:` line vs expected (normalized) |
| `webarena` | Stub only | Needs Docker + Playwright — see [`adapters/webarena/README.md`](adapters/webarena/README.md) |

Pass `--dataset path/to.json` to point at a larger downloaded set (same JSON shape as smoke files, or a loose `{ tasks: [...] }` wrapper). Official full leaderboard datasets are **not** vendored in git.

## Report schema

```json
{
  "schema": "deepstrike-capability-report/v0",
  "suite": "bfcl",
  "provider": "deepseek",
  "model": "deepseek-chat",
  "taskCount": 8,
  "passedCount": 6,
  "accuracy": 0.75,
  "meanScore": 0.78,
  "results": [ /* CapResult[] */ ]
}
```

Smoke scores are for regression and comparative runs — not official BFCL/GAIA/WebArena leaderboard numbers.

## Layout

```
capability/
├── core/           types, runner, report
├── adapters/
│   ├── bfcl/
│   ├── gaia/
│   └── webarena/   stub + env notes
├── cli/capability.mjs
└── README.md
```
