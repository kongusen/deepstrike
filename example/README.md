# Research Brief Studio — a DeepStrike curriculum

One product grown across eight levels, from a single sourced-Q&A agent into a multi-agent
editorial room. Each level is a self-contained, runnable project that **introduces one or two new
mechanisms while reusing everything before it** — so by the end you have exercised the whole
framework and, more importantly, seen the mechanisms *compose*.

The domain stays constant (research → brief), so every level differs only in the framework surface
it adds. The tools are local mocks (`search` / `read_source` over a canned corpus); the **provider
is real** — these examples run the live agent loop, not a scripted transcript.

## The ladder

| # | Project | New mechanisms | One-line idea |
|---|---|---|---|
| **L1** | Sourced Q&A assistant | Tools + Execution Plane · Provider · Session replay/recovery | one agent that answers with citations and resumes after a crash |
| **L2** | Assistant with memory | Memory (DreamStore, governed write gate, dedup, run-start recall) | remembers sources & preferences across sessions |
| **L3** | Assistant with a handbook | Skills (on-demand load + tool gating) · Knowledge | loads a "citation style" skill on demand; narrows the tools it exposes |
| **L4** | Event-driven assistant | Signals + Reactive (gateway, external triggers, injected notes) | a "new source arrived" webhook wakes it to process the delta |
| **L5** | Governed assistant | Governance · Resource Quota · OS Profile snapshot | forbids destructive tools, caps tokens/spawns, exports an observability snapshot |
| **L6** | Self-pacing digest | Loop Agent (`runLoop` / pace verbs / verdict gate / dormant→wake) | builds a running digest one source per round, pacing `continue`→`stop` itself |
| **L7** | Brief pipeline | Workflow DAG · Structured output + Reducer · Harness/Eval gate · Milestones | two researchers → deterministic reduce → writer → verify gate, every node schema-typed |
| **L8** | Editorial room | ReactiveSession (shared blackboard + turn policy) · RunGroup · DAG-in-peer | writer / editor / fact-checker peers collaborate under one cumulative budget |

## Mechanism coverage

| Mechanism | Level | Mechanism | Level |
|---|---|---|---|
| Tools + Execution Plane | L1 | Loop Agent / pace | L6 |
| Session replay & recovery | L1 | Structured output + Reducer | L7 |
| Provider routing | L1, L7 | Workflow DAG (5 node kinds) | L7 |
| Memory | L2 | Sub-agents / isolation / quarantine | L7 |
| Skills + tool gating | L3 | Harness / Eval quality gate | L7 |
| Knowledge | L3 | Milestones | L7 |
| Signals + Reactive | L4 | Context engineering (compaction/cache) | woven L2/L6 |
| Governance | L5 | RunGroup (cumulative budget + lineage) | L8 |
| Resource quota | L5 | ReactiveSession (blackboard + turn policy) | L8 |
| OS Profile / snapshots | L5 | | |

## Languages

Every level is **TypeScript** (the fullest SDK surface) **and Python** — each `main.ts` has a
`main.py` mirror using the snake_case Python SDK, so the whole curriculum runs cross-language. Run
the Python mirrors under `../python/.venv/bin/python <level>/main.py` (install once with
`pip install -e ../python`).

## Prerequisites

```sh
# build the SDK the TS examples import
npm run build --prefix ../node
# from this directory: link the local SDK (file:../node) + install tsx
npm install
```

Provider config comes from a `.env` file (auto-loaded from `example/.env`, then the repo root) or the
environment. Any one of:

```sh
ANTHROPIC_API_KEY=sk-ant-...                              # Anthropic
# — or an OpenAI-compatible endpoint —
OPENAI_API_KEY=sk-...
OPENAI_BASE_URL=https://your-endpoint/v1                  # optional
OPENAI_MODEL=gpt-5-mini                                   # optional
# DEEPSTRIKE_MODEL / DEEPSTRIKE_BASE_URL override either of the above.
```

The Python mirrors (L1, L8) run under `../python/.venv/bin/python` (install the SDK once with
`pip install -e ../python`). Every level accepts `--dry-run` to validate its wiring with **no key and
no call** — a fast way to confirm the setup before spending tokens. Start at
[`01-sourced-qa/`](./01-sourced-qa/).

## Status

**All eight levels are built, typechecked, and live-validated against a real provider** (each with a
`README.md`). L1 and L8 also ship a Python mirror (`main.py`), likewise live-run. Highlights from the
live runs:

- **L3** — `toolsExposed` visibly drops `6 → 5` the turn the `citation-style` skill activates (`list_index` gated away).
- **L4** — a gateway wire-alert *and* a high-urgency `injectNote` both reach a running loop and reshape the brief.
- **L5** — `publish_public` never appears (deny → schema pre-filter); the `email_editor` `ask_user` gate is host-adjudicated.
- **L6** — a 4-round loop paces itself `continue ×3 → stop`; the digest grows one line per round.
- **L7** — a 5-node DAG completes end to end: two research spawns → `concat` reducer → writer → verify gate, all schema-valid.
- **L8** — the `scribe`'s reaction is a whole workflow DAG, and the shared `RunGroup` ledger shows its `wf-node*` children billed alongside the reviewers' turns.

> Building **L6** surfaced (and fixed) a real SDK bug: the kernel-consumed `pace` meta-tool left an
> orphan `tool_call` in the replayed history, which strict OpenAI-compatible providers reject — so a
> paced loop died on round 2. The fix (`pairOrphanToolCalls` in the Node SDK, with regression tests)
> re-pairs kernel-consumed orphans while leaving genuinely-pending tail calls for wake/recovery. Full
> Node suite green (519). See [`06-daily-digest/README.md`](./06-daily-digest/README.md).
