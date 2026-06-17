<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>The agent kernel for dynamic workflows — Claude writes the harness, the kernel makes it replayable, governed, and cross-language.</strong>
</p>

<p align="center">
  <a href="https://github.com/kongusen/deepstrike/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kongusen/deepstrike?sort=semver&style=for-the-badge&label=release&labelColor=111827&color=374151"></a>
  <a href="https://www.npmjs.com/package/@deepstrike/sdk"><img alt="npm" src="https://img.shields.io/npm/v/@deepstrike/sdk?style=for-the-badge&logo=npm&logoColor=white&label=npm&labelColor=111827&color=374151"></a>
  <a href="https://pypi.org/project/deepstrike/"><img alt="PyPI" src="https://img.shields.io/pypi/v/deepstrike?style=for-the-badge&logo=pypi&logoColor=white&label=pypi&labelColor=111827&color=374151"></a>
  <a href="https://crates.io/crates/deepstrike-sdk"><img alt="crates.io" src="https://img.shields.io/crates/v/deepstrike-sdk?style=for-the-badge&logo=rust&logoColor=white&label=crates&labelColor=111827&color=374151"></a>
  <a href="https://discord.gg/cwS3RBYCv"><img alt="Discord" src="https://img.shields.io/badge/discord-community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=111827"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-374151?style=for-the-badge&labelColor=111827"></a>
</p>

<p align="center">
  <strong>English</strong>
  · <a href="./README.zh-CN.md">中文</a>
</p>

<p align="center">
  <a href="./docs/index.md">Documentation</a>
  · <a href="./docs/getting-started/quick-start.md">Quick Start</a>
  · <a href="./docs/guides/index.md">SDK Guides</a>
  · <a href="./docs/architecture/index.md">Architecture</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

---

> "Claude can now write its own harness on the fly, custom-built for the task at hand."
>
> — Thariq Shihipar & Sid Bidasaria, Anthropic Claude Code team, *A harness for every task: dynamic workflows in Claude Code*

That post named a real shift: instead of planning **and** executing a hard task in one long context window, the model writes a **dynamic workflow** — a small harness that spawns and coordinates separate sub-agents, each with its own clean context and a focused goal.

It matters because a single long context window reliably hits three failure modes (the article's terms):

- **Agentic laziness** — the model stops after partial progress (20 of 50 review items) and calls the job done.
- **Self-preferential bias** — it prefers its own results when asked to verify or judge them against a rubric.
- **Goal drift** — fidelity to the original objective decays across turns, especially after lossy compaction (the "don't do X" constraint quietly disappears).

The cure is structural: orchestrate **separate agents with their own context windows and isolated goals**. In Claude Code that harness is an ephemeral JavaScript file — so its orchestration state isn't replayable, isn't governed, and doesn't cross language boundaries.

**DeepStrike makes that harness a kernel primitive.** A workflow does two things — *control flow* (classify / fan-out / loop / barrier / tournament) and *I/O* (run an agent, search the web, read Slack). DeepStrike puts the control flow in a pure Rust kernel as **scheduling decisions**, and leaves the I/O in your host SDK:

```text
LLM emits a structured plan
        │
        ▼
deepstrike-core  ──  schedules nodes: gated · budgeted · replayable · resumable · cross-language
        │
        ▼
Host SDK (Node · Python · Rust · WASM)  ──  runs the agents, tools, worktrees, providers, I/O
```

Every node spawn passes the same syscall gate as a tool call, so quotas, trust boundaries, and token budgets apply **per node for free**. The orchestration state is serializable, snapshot-restorable, and behaves identically across all four host languages — strictly stronger than a script.

## The six harness patterns, as first-class kernel nodes

The article enumerates six composable patterns. Each is a first-class primitive in DeepStrike, driven by one workflow executor:

| Harness pattern (the article) | First-class in DeepStrike |
| :--- | :--- |
| **Classify-and-act** — a classifier routes to different agents | `NodeKind::Classify` — the classifier node's result selects one branch; the others are pruned before they ever run |
| **Fan-out-and-synthesize** — split, run an agent per step, merge at a barrier | `fanout_synthesize` — N parallel read-only workers → a synthesize barrier that waits for all and merges their structured outputs |
| **Adversarial verification** — verify each output against a rubric | `verify_rules` — one fresh-context verifier per rule, each in its own TCB with **no inherited author context**, so it can't rubber-stamp |
| **Generate-and-filter** — generate ideas, filter by rubric, dedupe | `generate_and_filter` — N generators → a `Verify` filter/dedupe barrier |
| **Tournament** — agents compete; pairwise judges pick a winner | `NodeKind::Tournament` — a controller node generates N entrants, then runs a pairwise judge bracket to one winner (comparative judgment beats absolute scoring; the deterministic loop holds the bracket) |
| **Loop until done** — loop until a stop condition, not a fixed pass count | `NodeKind::Loop` — re-run until the node reports done (`loop_continue`), with a hard `max_iters` backstop. For *unknown-size* discovery, a running node can also append fresh nodes to the live DAG with the `SubmitNodes` syscall (true loop-until-done · per-item fan-out) |

## The three failure modes, handled structurally

The point of the harness is to defeat the single-context failure modes by construction. DeepStrike enforces those mitigations in the kernel:

| Single-context failure mode | DeepStrike's structural answer |
| :--- | :--- |
| **Agentic laziness** — quits after partial progress | Each node runs in an isolated **TCB** with its own token budget; a `Loop` node carries an explicit stop condition **and** a hard `max_iters` cap, so "finish all 50" is enforced by structure, not by hope |
| **Self-preferential bias** — prefers its own output when judging | Verifiers and tournament judges run in a **separate TCB** with no inherited author context; a trust boundary keeps a node from grading its own work |
| **Goal drift** — loses the objective after compaction | A durable `task_state` plus a **directives channel** that survives renewal/compaction — exactly where ephemeral signals (and "don't do X" constraints) would otherwise be dropped |

## …and the article's other building blocks

The post also calls out mechanisms beyond the patterns. DeepStrike implements each in the kernel:

- **Quarantine, with no escape hatch** — the triage pattern bars agents that read *untrusted public content* from taking high-privilege actions. DeepStrike enforces this in-kernel: a `Quarantined` node that requests write-capable isolation is **denied at the syscall gate** (`NodeTrust`). And because a quarantined node may have read adversarial content, the *topology it asks for is untrusted too*: any nodes it submits at runtime are coerced to `Quarantined` (transitive taint), so it can't escape its sandbox by spawning a "trusted" child.
- **Deterministic compute between stages** — not every step needs an LLM. A `NodeKind::Reduce` node runs no agent: the kernel schedules it like a spawn, the SDK routes it to a pure registered function (`dedupe_lines` / `merge_json_arrays` / `concat` / `count`, or your own) over its dependencies' outputs. The dedupe/filter/merge "ordinary code between stages" of a script — but as a governed, replayable DAG node, with zero tokens burned.
- **Structured output, validated** — a node can declare an `output_schema`; the kernel carries it to the spawn descriptor, and the SDK instructs the agent, validates the result against it, and re-runs once with the errors fed back on mismatch. A node that never conforms **fails** (its dependents starve) rather than feeding garbage downstream.
- **Budget as a signal, not just a wall** — token/node budgets are enforced per spawn (`BudgetLedger`, `max_workflow_nodes`), *and* each spawned node learns its remaining headroom (`WorkflowBudget`), so a coordinator can size its next fan-out to the budget left instead of blindly hitting the cap.
- **Model & intelligence routing** — every node carries a `model_hint`; a Classify node can research a task and route it to a cheaper or stronger model (the article's Sonnet-vs-Opus example).
- **Resume after interruption** — quit the terminal mid-run and the workflow picks up where it left off — including runtime-appended nodes, recorded and replayed — via `WorkflowRun::resume` and a rebuildable `KernelSnapshot`.

## Why a kernel, not a script

A dynamic workflow as a JavaScript harness is powerful but ephemeral. Lifting the control flow into a kernel buys properties a script can't have:

- **Replayable** — the control-flow state is a serializable state machine; replay reconstructs a run and strips audit events when rebuilding LLM messages.
- **Governed** — every node spawn flows through the same in-kernel policy as a tool call: quotas, capability checks, trust, vetoes, rate limits, audit.
- **Resumable** — interrupted DAGs restore from the session log / `KernelSnapshot`, not from scratch.
- **Cross-language** — one kernel drives Node, Python, Rust, and WASM hosts with identical semantics.
- **Host-owned I/O** — providers, tools, worktrees, network, and storage stay in your SDK; the kernel only decides *when* and *whether*.

## A dynamic workflow, end to end

The article's *memory & rule-adherence* use case — "verify every technical claim, one verifier per rule, plus a skeptic" — is a workflow DAG you hand to the kernel. The host runs the agents; the kernel gates each spawn, suspends on the join, and advances on completion:

```ts
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "@deepstrike/sdk"
import { AnthropicProvider } from "@deepstrike/sdk"

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(process.env.ANTHROPIC_API_KEY!),
  executionPlane: new LocalExecutionPlane(),
  sessionLog: new InMemorySessionLog(),
  maxTokens: 32_000,
})

// One fresh-context verifier per rule (no inherited author context → can't rubber-stamp),
// then a skeptic that reviews their flags to suppress false positives.
const spec = {
  nodes: [
    { task: "Rule: money is integer cents — is it violated in the code?", role: "verify" },
    { task: "Rule: all errors propagate — is it violated?",              role: "verify" },
    { task: "Rule: timestamps are UTC — is it violated?",                role: "verify" },
    { task: "Skeptic: of the flags above, which are real violations?",   role: "verify", dependsOn: [0, 1, 2] },
  ],
}

// Kernel spawns the 3 verifiers as one gated batch, suspends on the join,
// then runs the skeptic once they complete — replayable, resumable, audited.
const outcome = await runner.runWorkflow(spec)
```

Swap a node's `kind` to `{ type: "loop", maxIters: 5 }`, `{ type: "classify", branches: [...] }`, `{ type: "tournament", entrants: [...] }`, or `{ type: "reduce", reducer: "dedupe_lines" }` and the same executor drives loops, conditional routing, pairwise brackets, and tokenless host-compute — every node still passing the syscall gate. Hand a node the `submit_workflow_nodes` tool and it can grow the DAG mid-run (one verifier per claim it discovers).

**0.2.11** — Dynamic workflows go runtime-dynamic: the `SubmitNodes` syscall lets a running node grow the DAG (true loop-until-done · per-item fan-out), plus deterministic `Reduce` nodes, per-node `output_schema`, budget-as-signal, and a quarantine no-escalation gate. See the [CHANGELOG](./CHANGELOG.md).

## Built on an Agent OS substrate

What makes the workflow story credible is the kernel underneath it — the same machinery that gates a tool call gates a node spawn:

- **Kernel-mediated runtime (M0–M4)** — tool calls, spawns, compression, and signals pass one syscall gate with an explicit lifecycle (Ready / Running / Blocked / Suspended). You implement I/O; the kernel decides *when* and *whether*.
- **Longer, sturdier sessions** — oversized tool results stay in context as a preview plus a `.spool/` reference; semantic page-out archives summaries into long-term memory and serves page-in on the way back.
- **Safety & governance by default** — every run loads declarative governance (deny / ask_user / rate-limit / param rules) and in-kernel signal disposition (Interrupt / Queue / Observe / Dropped). Policy, not ad-hoc checks.
- **Long-term memory as syscalls** — `writeMemory` / `queryMemory` with validation before commit and an auditable search → selection → retrieval closure.
- **Process table & multi-signal orchestration** — sub-agents register in the kernel process table; parents suspend until join; external signals compose with the loop instead of racing it.
- **Observable like an OS log** — spool, page-out, signals, processes, budgets, and memory events land in session logs by category (`syscall` · `sched` · `mm` · `proc` · `ipc`); rebuild OS snapshots from one event stream.

SDK-specific APIs and examples: [Node.js](./node/README.md#what-agent-os-gives-you) · [Python](./python/README.md#what-agent-os-gives-you) · [Rust](./docs/guides/sdk-rust.md)

## Language and runtime support

| Runtime | Package | Install |
| :--- | :--- | :--- |
| Node.js / TypeScript | `@deepstrike/sdk` | `npm install @deepstrike/sdk` |
| Python | `deepstrike` | `pip install deepstrike` |
| Rust | `deepstrike-sdk` | `cargo add deepstrike-sdk` |
| Browser / Edge / WASM | `@deepstrike/wasm` | `npm install @deepstrike/wasm` |

Current workspace version: `0.2.11`.

## Quick start

### Node.js / TypeScript

```bash
npm install @deepstrike/sdk
```

```ts
import {
  AnthropicProvider,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

const schema = JSON.stringify({
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
})

const add = tool("add", "Add two numbers.", schema, async ({ x, y }) => {
  return String((x as number) + (y as number))
})

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(process.env.ANTHROPIC_API_KEY!),
  executionPlane: new LocalExecutionPlane().register(add),
  sessionLog: new InMemorySessionLog(),
  maxTokens: 32_000,
})

const answer = await collectText(
  runner.run({ sessionId: "demo", goal: "What is 2 + 3?" }),
)
```

### Python

```bash
pip install deepstrike
```

```py
from deepstrike import (
    AnthropicProvider,
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
def add(x: int, y: int) -> int:
    """Add two numbers."""
    return x + y

runner = RuntimeRunner(RuntimeOptions(
    provider=AnthropicProvider(api_key="..."),
    execution_plane=LocalExecutionPlane().register(add),
    session_log=InMemorySessionLog(),
    max_tokens=32_000,
))

answer = await collect_text(runner.run_streaming("What is 2 + 3?"))
```

### Rust

```toml
[dependencies]
deepstrike-sdk = "0.2.24"
```

See the [SDK guides](./docs/guides/index.md) for full examples, provider configuration, streaming events, and governance hooks. For the dynamic-workflow drive (`runWorkflow` / `run_workflow`), see the per-SDK **Dynamic workflows** sections: [Node.js](./node/README.md#dynamic-workflows) · [Python](./python/README.md#dynamic-workflows).

## Documentation

| Reader path | Start here |
| :--- | :--- |
| New users | [Quick Start](./docs/getting-started/quick-start.md) |
| SDK users | [Node.js](./docs/guides/sdk-nodejs.md), [Python](./docs/guides/sdk-python.md), [Rust](./docs/guides/sdk-rust.md), [WASM](./docs/guides/index.md) |
| Runtime designers | [Agent OS](./docs/concepts/agent-os.md) · [Core Concepts](./docs/concepts/core-concepts.md) |
| Architecture reviewers | [Architecture Overview](./docs/architecture/overview.md) |
| Integrators | [Provider Guide](./docs/guides/providers.md) and [Kernel ABI](./docs/reference/kernel-abi.md) |
| Operators | [Release Runbook](./docs/operations/release-runbook.md) |
| Contributors | [Contributing Guide](./CONTRIBUTING.md) |

```bash
npm install
npm run docs:dev      # local docs site
npm run docs:build    # static build
```

## Repository layout

```text
crates/deepstrike-core/   Pure Rust kernel state machine (workflow executor lives here)
crates/deepstrike-node/   Node.js native bindings
crates/deepstrike-py/     Python native bindings
crates/deepstrike-wasm/   WASM bindings
node/                     TypeScript host SDK
python/                   Python host SDK
rust/                     Rust host SDK
wasm/                     Browser and edge SDK
docs/                     VitePress documentation source
tests/                    Cross-language integration tests
scripts/                  Release and verification automation
```

## Local development

Requirements: Rust 1.85+ · Node.js 18+ · Python 3.10+

```bash
cargo build && cargo test
```

```bash
cd node && npm install && npm run build && npm test
```

```bash
cd python && python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest pytest-asyncio && maturin develop --release && pytest
```

```bash
cd wasm && npm install && npm run build && npm test
```

## Community

- Join the developer community on [Discord](https://discord.gg/cwS3RBYCv).
- Report issues or request features in [GitHub Issues](https://github.com/kongusen/deepstrike/issues).
- Read [CONTRIBUTING.md](./CONTRIBUTING.md) before opening a pull request.
- Report security issues through the process in [SECURITY.md](./SECURITY.md).

## License

DeepStrike is released under the [MIT License](./LICENSE). DeepStrike is an independent open-source project inspired by Anthropic's published work on dynamic workflows in Claude Code; it is not affiliated with or endorsed by Anthropic.
