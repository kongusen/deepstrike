<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>An Agent OS microkernel for dynamic workflows, governed tools, replayable sessions, and cross-language agent runtimes.</strong>
</p>

<p align="center">
  <a href="https://github.com/kongusen/deepstrike/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kongusen/deepstrike?sort=semver&style=for-the-badge&label=release&labelColor=111827&color=374151"></a>
  <a href="https://www.npmjs.com/package/@deepstrike/sdk"><img alt="npm" src="https://img.shields.io/npm/v/@deepstrike/sdk?style=for-the-badge&logo=npm&logoColor=white&label=npm&labelColor=111827&color=374151"></a>
  <a href="https://pypi.org/project/deepstrike/"><img alt="PyPI" src="https://img.shields.io/pypi/v/deepstrike?style=for-the-badge&logo=pypi&logoColor=white&label=pypi&labelColor=111827&color=374151"></a>
  <a href="https://crates.io/crates/deepstrike-sdk"><img alt="crates.io" src="https://img.shields.io/crates/v/deepstrike-sdk?style=for-the-badge&logo=rust&logoColor=white&label=crates&labelColor=111827&color=374151"></a>
  <a href="https://www.anthropic.com/claude"><img alt="Optimized with Fable 5" src="https://img.shields.io/badge/optimized%20with-Fable%205-D97757?style=for-the-badge&logo=anthropic&logoColor=white&labelColor=111827"></a>
  <a href="https://discord.gg/cwS3RBYCv"><img alt="Discord" src="https://img.shields.io/badge/discord-community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=111827"></a>
  <a href="https://x.com/w73775"><img alt="Follow @w73775 on X" src="https://img.shields.io/badge/follow-%40w73775-000000?style=for-the-badge&logo=x&logoColor=white&labelColor=111827"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-374151?style=for-the-badge&labelColor=111827"></a>
</p>

<p align="center">
  <strong>English</strong>
  · <a href="./README.zh-CN.md">中文</a>
</p>

<p align="center">
  <a href="./docs/en/index.md">Documentation</a>
  · <a href="./docs/en/getting-started/hello-agent.md">Hello Agent</a>
  · <a href="./docs/en/architecture/agent-os.md">Agent OS</a>
  · <a href="./docs/en/guides/workflow.md">Dynamic Workflows</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

<details>
<summary><strong>Explore DeepStrike</strong></summary>

- [Start here](#start-here)
- [See it in action](#see-it-in-action)
- [What you get](#what-you-get)
- [Why a kernel?](#why-a-kernel)
- [Quick start](#quick-start)
- [Dynamic workflow patterns](#dynamic-workflow-patterns)
- [When DeepStrike fits](#when-deepstrike-fits)
- [Documentation](#documentation)
- [FAQ](#frequently-asked-questions)

</details>

---

DeepStrike turns an agent "harness" into a kernel primitive.

Modern agents increasingly solve hard tasks by writing a small workflow: classify the work, fan out to sub-agents, verify outputs, loop until done, and synthesize a final answer. In a script, that harness is powerful but fragile: state lives in process memory, governance is ad hoc, recovery is hard, and every language has to reimplement the same semantics.

DeepStrike moves the control plane into `deepstrike-core`, a pure Rust state machine. Host SDKs still own all real I/O: LLM calls, tools, files, worktrees, network, long-term memory, and storage. The kernel decides when and whether effects may happen; the host executes approved effects and feeds observations back.

<p align="center">
  <img src="docs/public/readme_agent_os_map.svg" alt="DeepStrike runtime mechanism: host-owned I/O, RuntimeRunner, kernel, SDKs, and self-harness loop" width="100%" />
</p>

## Start Here

DeepStrike is for builders who want agent autonomy **without moving authority into the prompt**. Start with the path that matches what you are building:

| I want to... | Start with... | What it demonstrates |
| :--- | :--- | :--- |
| Run one tool-using agent | [Hello Agent](./docs/en/getting-started/hello-agent.md) | Provider setup, tools, streaming, and the first durable session |
| Build a multi-agent pipeline | [Brief Pipeline](./example/07-brief-pipeline/) | A typed five-node DAG with parallel research, deterministic reduce, writing, and verification |
| Put hard policy around side effects | [Governed Studio](./example/05-governed-studio/) | Deny-before-exposure, ask-user suspension, quotas, and an auditable OS snapshot |
| Build a long-running or recoverable agent | [Daily Digest](./example/06-daily-digest/) | A self-pacing loop, durable rounds, verdict gates, dormancy, and wake |
| Compare the SDK surfaces | [Node.js](./node/README.md) · [Python](./python/README.md) · [Rust](./rust/README.md) · [WASM](./wasm/README.md) | One Rust kernel ABI with host-native APIs |

Not sure which API to choose? Use [`runAgent`](./docs/en/getting-started/run-agent-vs-runner.md) for the shortest path, `runFanout` for a governed one-shot workflow, and `RuntimeRunner` when you need explicit control over tools, sessions, signals, memory, governance, or workflow execution.

## See It in Action

The repository includes **Research Brief Studio**, an eight-level, runnable curriculum. It starts with one sourced-Q&A agent and grows the same product into a governed, replayable, multi-agent editorial room. Each level adds one or two mechanisms, keeps the domain constant, and includes a no-key `--dry-run` path.

| Level | Runnable project | What changes |
| :---: | :--- | :--- |
| L1 | [Sourced Q&A](./example/01-sourced-qa/) | Tools, provider execution, SessionLog replay, and crash recovery |
| L2 | [Memory Assistant](./example/02-memory-assistant/) | Governed memory writes, deduplication, and run-start recall |
| L3 | [Skills Handbook](./example/03-skills-handbook/) | On-demand skills, capability gating, and pinned knowledge |
| L4 | [Reactive Desk](./example/04-reactive-desk/) | External signals, injected notes, and an urgency-aware attention policy |
| L5 | [Governed Studio](./example/05-governed-studio/) | Allow / deny / ask-user policy, resource quotas, and OS snapshots |
| L6 | [Daily Digest](./example/06-daily-digest/) | Self-pacing rounds, verdict gates, dormant state, and wake |
| L7 | [Brief Pipeline](./example/07-brief-pipeline/) | Typed workflow DAGs, isolated sub-agents, reducers, and a verifier gate |
| L8 | [Editorial Room](./example/08-editorial-room/) | Reactive peers, a shared blackboard, one cumulative budget, and DAG-in-peer composition |

```bash
# Build the local Node SDK and validate the capstone wiring without an API key.
npm run build --prefix node
cd example && npm install
npx tsx 08-editorial-room/main.ts --dry-run
```

Read the [full curriculum map](./example/README.md) for prerequisites, provider configuration, Python mirrors, and live-run notes.

## What You Get

| Capability | What DeepStrike provides |
| :--- | :--- |
| **Dynamic workflow scheduler** | Declarative DAGs plus runtime `SubmitNodes`; first-class `Loop`, `Classify`, `Tournament`, `Reduce`, fan-out, synthesize, generate-filter, and verifier patterns. |
| **Unified syscall governance** | Tool calls, sub-agent spawn, workflow growth, and memory writes pass one gate with allow / deny / ask-user / rate-limit / quota dispositions. |
| **Context VM** | Four-slot rendering (`system_stable`, `system_knowledge`, `turns`, `state_turn`), pressure compression, handle paging for large tool results, prompt-cache-aware stable prefixes, and a governed knowledge lifecycle (keyed entries, boundary-deferred eviction, knowledge budget, skill leases). |
| **Sub-agent isolation** | Roles, context inheritance, capability filters, worktree / read-only / remote isolation, process lineage, contracts, and handoff artifacts. |
| **Replay and recovery** | Append-only `SessionLog`, provider replay envelopes, kernel observations, workflow resume, `wake(session_id)`, OS snapshots, and repair utilities. |
| **Memory as an OS device** | Kernel-validated `write_memory` / `query_memory`, DreamStore integration, retrieval closure, idle consolidation, and memory write quotas. |
| **Self-improving harness lab** | Node-first, content-addressed `HarnessManifest` profiles, declarative instruction and nudge surfaces, verifier-anchored failure mining, held-in/held-out validation, and auditable propose–validate–promote lineage. |
| **Provider routing** | Kernel carries `model_hint`; the host resolves it to OpenAI, Anthropic, Gemini, DeepSeek, Kimi, Qwen, GLM, Minimax, Ollama, or your own provider. |
| **Multimodal input** | Image and audio via `run({ attachments })` across all four SDKs, per-vendor serialization (Anthropic blocks, OpenAI `image_url` / `input_audio`, Gemini `inlineData`), detail-weighted token accounting, and `UnsupportedModalityError` instead of silent drops. |
| **Cross-language runtime** | One kernel ABI and matching semantics across Node.js, Python, Rust, and WASM. |

## Why a Kernel?

The Agent OS split is deliberately narrow:

```text
LLM emits a plan or tool request
        |
        v
deepstrike-core decides: schedule, gate, budget, compress, snapshot
        |
        v
Host SDK executes: provider, tools, files, worktrees, stores, webhooks
        |
        v
Observations return to the kernel and SessionLog
```

That boundary gives you properties a one-off orchestrator script does not:

| Property | Script harness | DeepStrike kernel |
| :--- | :--- | :--- |
| Replay | State is usually closure variables or temporary files | Control-flow observations and snapshots rebuild the run |
| Governance | Each tool path implements checks differently | One syscall gate covers tools, spawn, memory, and workflow append |
| Recovery | Interruptions often restart the harness | SessionLog + `KernelSnapshot` restore suspended workflows |
| Cross-language | Semantics drift across SDKs | Rust kernel drives every host |
| I/O ownership | Control flow and credentials mix together | Kernel is pure compute; host owns credentials and side effects |

## Runtime Layers

| Layer | Owns | Does not own |
| :--- | :--- | :--- |
| **Kernel (`deepstrike-core`)** | State machine, scheduling, syscall disposition, governance, workflow DAGs, budget ledger, context rendering, memory validation, observations | HTTP, filesystem, provider clients, vector stores, subprocesses |
| **Host SDK** | Runtime loop, provider calls, tool execution, session persistence, DreamStore, archive store, worktree and sandbox integration | Reimplementing spawn gates or workflow semantics |
| **Provider** | Vendor protocol adaptation, streaming, replay envelopes, model-specific runtime policy | Policy decisions |
| **ExecutionPlane** | Local tools, streaming tools, suspend/resume, worktree cwd injection, process sandbox, remote VPC tools, large result spool | Context compression |

### Mechanisms You Can Inspect

DeepStrike keeps its important behavior explicit: workflow growth is data, side effects enter through one governance funnel, context pressure has a visible policy, and recovery is derived from an evidence journal.

<p align="center">
  <img src="docs/public/workflow_mechanisms.svg" alt="Dynamic workflow DAG growth, control nodes, scheduling, and quota mechanisms" width="100%" />
</p>

<p align="center">
  <img src="docs/public/governance_pipeline.svg" alt="Unified syscall governance funnel for tools, sub-agents, and memory writes" width="100%" />
</p>

<p align="center">
  <img src="docs/public/session_replay_mechanisms.svg" alt="Append-only session journal, wake and resume, and deterministic replay paths" width="100%" />
</p>

Open the [complete System Diagram Atlas](./docs/en/architecture/diagram-atlas.md), or explore the corresponding guides: [Workflows](./docs/en/guides/workflow.md) · [Governance](./docs/en/guides/governance.md) · [Session, Replay & Recovery](./docs/en/guides/session-replay-and-recovery.md) · [Context Engineering](./docs/en/guides/context-engineering.md).

## Install

| Runtime | Package | Install |
| :--- | :--- | :--- |
| Node.js / TypeScript | `@deepstrike/sdk` | `npm install @deepstrike/sdk` |
| Python | `deepstrike` | `pip install deepstrike` |
| Rust | `deepstrike-sdk` | `cargo add deepstrike-sdk` |
| Browser / Edge / WASM | `@deepstrike/wasm` | `npm install @deepstrike/wasm` |

Current SDK version in this workspace: `0.2.48`.

## Quick Start

### Node.js / TypeScript

```bash
npm install @deepstrike/sdk
```

```ts
import { OpenAIProvider, runAgent, runFanout, tool } from "@deepstrike/sdk"

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const provider = new OpenAIProvider({
  apiKey: process.env.OPENAI_API_KEY!,
  model: "gpt-4.1-mini",
})

const answer = await runAgent({
  provider,
  goal: "What is 17 + 28?",
  tools: [add],
})

const { synthesis } = await runFanout({
  provider,
  tasks: [
    "Summarize the auth module's risk profile.",
    "Summarize the data layer's risk profile.",
  ],
  synthesize: "Combine the findings into one concise review.",
})
```

Use `runAgent` for the simple path, `runFanout` for a kernel-gated workflow from a stateless handler, and `RuntimeRunner` when you need streaming events, SessionLog persistence, tools, governance, signals, memory, or explicit workflow control.

### Python

```bash
pip install deepstrike
```

```py
from deepstrike import OpenAIProvider, run_agent, run_fanout, tool

@tool
async def add(x: int, y: int) -> str:
    """Add two numbers."""
    return str(x + y)

provider = OpenAIProvider(api_key="sk-...", model="gpt-4.1-mini")

answer = await run_agent(
    provider=provider,
    goal="What is 17 + 28?",
    tools=[add],
)

out = await run_fanout(
    provider=provider,
    tasks=[
        "Summarize the auth module's risk profile.",
        "Summarize the data layer's risk profile.",
    ],
    synthesize="Combine the findings into one concise review.",
)
synthesis = out["synthesis"]
```

### Rust

```toml
[dependencies]
deepstrike-sdk = "0.2.48"
```

### WASM

```bash
npm install @deepstrike/wasm
```

See the per-runtime READMEs for full examples: [Node.js](./node/README.md), [Python](./python/README.md), [Rust](./rust/README.md), [WASM](./wasm/README.md).

## Dynamic Workflow Patterns

DeepStrike implements the common harness patterns as first-class workflow nodes rather than prompt-only conventions.

| Pattern | Kernel / SDK surface |
| :--- | :--- |
| Classify and act | `classify` node selects one branch and prunes the rest |
| Fan out and synthesize | `runFanout` / `fanout_synthesize`: N workers plus synthesis barrier |
| Adversarial verification | `verify_rules`: one fresh-context verifier per rule |
| Generate and filter | `generate_and_filter`: parallel generators plus verifier barrier |
| Tournament | `tournament` node with pairwise judging |
| Loop until done | `loop` node with `loop_continue`, `max_iters`, and runtime `SubmitNodes` |
| Deterministic compute | `Reduce` node with reducers such as `concat`, `dedupe_lines`, `merge_json_arrays`, and `count` |

Read the workflow guide: [Dynamic Workflows](./docs/en/guides/workflow.md).

## When DeepStrike Fits

DeepStrike is a strong fit when your agent system needs one or more of these properties:

- **Control flow must survive process boundaries.** Sessions, workflow observations, and snapshots need to resume in another worker instead of living only in a closure.
- **Side effects need enforceable policy.** Tools, sub-agent creation, workflow growth, and memory writes must share quotas and allow / deny / ask-user decisions.
- **The harness changes at runtime.** The model may classify, fan out, append nodes, loop, reduce, verify, or hand work to an isolated child while the host retains authority.
- **Multiple languages must agree on semantics.** Node.js, Python, Rust, and WASM need one scheduling and governance contract rather than parallel reimplementations.
- **Runs need to be explainable.** Provider envelopes, kernel observations, tool outcomes, permissions, and recovery boundaries must be available for replay and audit.

DeepStrike may be more machinery than you need for a stateless chatbot, a single prompt with no tools, or a short script where restart-from-zero is acceptable. Start with a provider SDK directly in those cases; adopt a kernel boundary when durability, governance, dynamic orchestration, or cross-runtime consistency becomes a real requirement.

## Self-Improving Harnesses (Experimental)

The Node SDK exposes the model-visible harness as bounded data rather than arbitrary middleware code:

- `RuntimeOptions.instructions` provides ordered `bootstrap`, `execution`, `verification`, and `failureRecovery` slots while the kernel still receives one byte-stable system prompt.
- `RuntimeOptions.nudges` maps runtime events such as tool errors, denials, turn thresholds, and entropy alerts onto the existing `injectNote` signal path.
- `HarnessManifest` and `HarnessPatch` provide canonical digests, parent-linked lineage, and an explicit editable-surface whitelist. Governance, quota, and reliability controls are not proposer-editable.

The repository also includes a Node-first self-harness lab based on a conservative propose–validate–promote loop. It clusters verifier-anchored failures, asks the fixed target model to propose minimal JSON patches, evaluates every candidate on held-in and held-out task splits, and promotes only non-regressing improvements:

```text
accept when Δ_in >= 0 and Δ_held_out >= 0 and at least one delta is positive
```

Run the included live format-discipline example after configuring a supported provider:

```bash
node benchmark/selfharness/cli.mjs \
  --adapter ./benchmark/selfharness/adapters/format-discipline.mjs \
  --held-in json-strict,word-limit,checklist \
  --held-out csv-strict,summary-limit \
  --rounds 2 --k 3 --repeats 2 --provider deepseek
```

Every promoted manifest is stored as `<digest>.json`, with per-round proposals and decisions in `rounds.jsonl`. Held-out task content never enters the miner or proposer prompt. This lab is currently Node.js-only; production profile generation requires a representative task corpus, repeated evaluation, and real provider budget. See the [benchmark guide](./benchmark/README.md#self-harness-lab--selfharness) and the [accepted design spec](./.local-docs/specs/self-harness-loop.md).

## Documentation

| Reader path | Start here |
| :--- | :--- |
| New users | [Hello Agent](./docs/en/getting-started/hello-agent.md) and [Choosing an API](./docs/en/getting-started/run-agent-vs-runner.md) |
| Runtime designers | [What is Agent OS?](./docs/en/architecture/agent-os.md), [Kernel / SDK Split](./docs/en/architecture/overview.md), [Execution Model](./docs/en/architecture/execution-model.md) |
| Workflow builders | [Dynamic Workflows](./docs/en/guides/workflow.md), [Sub-Agents & Collaboration](./docs/en/guides/sub-agents-and-collaboration.md), [Structured Output & Reducers](./docs/en/guides/structured-output-and-reducers.md) |
| Production integrators | [Execution Plane & Tools](./docs/en/guides/execution-plane-and-tools.md), [Governance](./docs/en/guides/governance.md), [Provider Routing](./docs/en/guides/provider-routing.md) |
| Long-context agents | [Context Engineering](./docs/en/guides/context-engineering.md), [Memory](./docs/en/guides/memory.md), [Prompt Cache Design](./docs/en/concepts/prompt-cache-design.md) |
| Replay and operations | [Session, Replay & Recovery](./docs/en/guides/session-replay-and-recovery.md), [OS Profile & Runtime Snapshots](./docs/en/guides/os-profile-and-snapshots.md), [Signals & Reactive](./docs/en/guides/signals-and-reactive.md) |
| Reference | [RuntimeOptions](./docs/en/reference/runtime-options.md), [WorkflowNodeSpec](./docs/en/reference/workflow-node-spec.md), [Python API](./docs/en/reference/python-api.md), [Kernel ABI](./docs/en/architecture/kernel-abi.md) |

Run the docs locally:

```bash
npm install
npm run docs:dev
npm run docs:build
```

## Reliability by Construction

DeepStrike treats reliability as runtime state, not a prompt-writing convention:

| Concern | Mechanism |
| :--- | :--- |
| Interrupted execution | Append-only `SessionLog`, kernel observations, `KernelSnapshot`, `wake(session_id)`, and workflow resume |
| Provider nondeterminism | Recorded provider replay envelopes and `ReplayProvider` paths for network-free validation |
| Unsafe capabilities | Schema pre-filtering, one syscall gate, parameter constraints, quotas, and suspendable ask-user decisions |
| Context overflow | Four-slot Context VM, token-pressure compaction, large-result handles, and prompt-cache-aware stable prefixes |
| Untrusted delegation | Capability filters, quarantine, context inheritance controls, worktree / read-only / remote isolation, and lineage |
| Semantic drift across SDKs | A shared Rust kernel ABI plus cross-language integration tests |

For the contracts behind these claims, see the [runtime reliability ADR](./docs/en/decisions/001-runtime-reliability-contracts.md), [kernel ABI reliability ADR](./docs/en/decisions/002-kernel-abi-reliability.md), and [kernel performance baseline](./docs/en/architecture/kernel-performance-baseline.md).

## Repository Layout

```text
benchmark/                Evaluation scenarios, replay baselines, and self-harness lab
crates/deepstrike-core/   Pure Rust kernel state machine
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

## Local Development

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

## Frequently Asked Questions

<details>
<summary><strong>Is DeepStrike another agent framework?</strong></summary>

It is narrower than an application framework. `deepstrike-core` is a pure state-machine kernel for scheduling, governance, context, budgets, observations, and recovery. Host SDKs keep ownership of providers, tools, files, credentials, storage, and every real side effect.

</details>

<details>
<summary><strong>Do I have to use multi-agent workflows?</strong></summary>

No. `runAgent` is the simple single-agent path. You can add `RuntimeRunner`, fan-out, loops, workflow DAGs, or reactive peers only when the application needs them.

</details>

<details>
<summary><strong>Which model providers are supported?</strong></summary>

The host SDK includes adapters or OpenAI-compatible routing for OpenAI, Anthropic, Gemini, DeepSeek, Kimi, Qwen, GLM, Minimax, Ollama, and custom providers. The kernel only carries a `model_hint`; credentials and vendor protocol details never move into the kernel.

</details>

<details>
<summary><strong>Can a denied tool call still execute?</strong></summary>

No. Governance is enforced below the model. Denied tools can be removed before the provider sees the schema; gated calls suspend until the host returns a decision; approved effects execute only in the host's `ExecutionPlane`.

</details>

<details>
<summary><strong>How does recovery work?</strong></summary>

The host persists an append-only `SessionLog`. On wake or resume, DeepStrike folds recorded observations to rebuild runtime state and continues from the durable boundary. Provider replay envelopes also let tests reproduce recorded model behavior without another network call.

</details>

<details>
<summary><strong>Can I try the repository without spending model tokens?</strong></summary>

Yes. Every level in the [Research Brief Studio curriculum](./example/README.md) accepts `--dry-run` to validate configuration and wiring without an API key or provider call.

</details>

## License

DeepStrike is released under the [MIT License](./LICENSE). DeepStrike is an independent open-source project inspired by published work on dynamic workflows in agent coding tools; it is not affiliated with or endorsed by Anthropic.
