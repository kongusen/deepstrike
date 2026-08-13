<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>A local Agent Process Runtime for durable, governed work.</strong>
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
  · <a href="./docs/en/index.md">Documentation</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

---

DeepStrike is a local Agent Process Runtime for building Agents that can do more than answer a prompt. Give an Agent a model, instructions, tools, MCP servers, skills, memory, knowledge, and handoffs. The runtime gives its work a durable process tree so it can continue across turns, coordinate other Agents, wait for external input, and recover after interruption.

The framework keeps the public Agent model familiar while the kernel makes lifecycle, authority, resources, scheduling, communication, and recovery explicit, composable, and testable.

## Agent Process Runtime

Every root Agent run, child Agent, and workflow node participates in one local runtime model:

| Runtime responsibility | What it guarantees |
| --- | --- |
| **Process lifecycle** | Kernel-derived parent-child lineage with consistent spawn, join, cancel, and supervision semantics. |
| **Durable scheduling** | Deterministic runnable selection and persistent waits for effects, children, approvals, signals, timers, channels, and resources. |
| **Authority and resources** | Child capabilities can only narrow; nine-dimensional budget grants cannot exceed parent remaining capacity. |
| **Communication and recovery** | Capability-checked local IPC, handle-only large objects, checkpoints, journals, and replay-safe continuation. |

“Process” is a runtime abstraction, not a requirement to launch one operating-system process per Agent. The SDK remains the public API; the runtime kernel enforces the invariants behind it. See [Agent Process Runtime](./docs/en/architecture/agent-process-runtime.md) for the complete model and its local-only boundary.

## What An Agent Can Do

| Agent capability | What DeepStrike provides |
| --- | --- |
| **Reason** | OpenAI, Anthropic, Gemini, DeepSeek, Kimi, Qwen, GLM, Minimax, Ollama, and custom provider integrations with streaming and replay support. |
| **Use tools** | Typed tools, streaming tools, MCP integrations, local execution, worktrees, process sandboxes, and remote tool adapters. |
| **Remember** | Working memory for the current run, durable MemoryStore integrations, session extraction, retrieval, and governed memory writes. |
| **Load knowledge** | Skills and knowledge sources that can be loaded when needed, pinned for a run, budgeted, and released as the Agent changes tasks. |
| **Delegate** | Child Agents with explicit roles, narrowed capabilities, isolated context, handoffs, contracts, and lineage. |
| **Coordinate** | Parallel fan-out, synthesis, dependency graphs, classifiers, reducers, verifier gates, tournaments, and bounded loops. |
| **Wait and wake** | Approval requests, child completion, external signals, and resumable sessions without rebuilding the whole conversation manually. |
| **Stay within limits** | Tool policies, parameter constraints, approval gates, rate limits, quotas, budgets, and cancellation rules. |
| **Handle long context** | Stable instructions, knowledge, conversation history, state, compression, and large-result paging that keep prompts usable over long runs. |
| **Explain what happened** | Session logs, structured events, replay fixtures, snapshots, and recovery evidence for debugging and audit. |

These capabilities compose around one Agent contract. You can start with a single tool call and add memory, skills, delegation, workflow control, or persistence only when the application needs them.

## Quick Start

Install the Node.js SDK:

```bash
npm install @deepstrike/sdk
```

Run an Agent with a typed tool and a local session log:

```ts
import {
  FileSessionLog,
  LocalExecutionPlane,
  OpenAIResponsesProvider,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const runner = new RuntimeRunner({
  provider: new OpenAIResponsesProvider(process.env.OPENAI_API_KEY!, "gpt-5-mini"),
  executionPlane: new LocalExecutionPlane().register(add),
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
})

const answer = await collectText(runner.run({
  sessionId: "math-1",
  goal: "What is 17 + 28?",
}))

console.log(answer)
```

Choose the smallest entry point that fits your Agent:

| Need | Entry point |
| --- | --- |
| One goal, optional tools, and final text | `runAgent` / `run_agent` |
| Parallel Agents followed by synthesis | `runFanout` / `run_fanout` |
| Streaming, sessions, memory, signals, governance, or explicit workflows | `RuntimeRunner` |

For language-specific installation and examples, see [Node.js](./node/README.md), [Python](./python/README.md), [Rust](./rust/README.md), and [WASM](./wasm/README.md). Start with the [Hello Agent guide](./docs/en/getting-started/hello-agent.md).

## Common Agent Patterns

### One Agent with tools

Register typed tools and let the Agent decide when to call them. Tool schemas, execution results, errors, and streaming events are part of the run.

### Memory assistant

Attach a `MemoryStore` to recall durable user or project facts, write new memories through a policy, and extract useful records when a session ends.

### Skill-based Agent

Keep specialized instructions and tools in skills. Load a skill when the task needs it, keep its knowledge available for the active phase, and release it when the Agent moves on.

### Multi-Agent workflow

Describe tasks as a graph. Run research Agents in parallel, reduce their outputs deterministically, ask a verifier to challenge the result, and synthesize the final response.

### Long-running Agent

Persist the session, pause for approval or external events, and resume with `wake(sessionId)`. Checkpoints and replay evidence let the application continue after a process restart.

The [Research Brief Studio curriculum](./example/README.md) demonstrates these patterns in eight runnable levels, from sourced Q&A to a governed multi-Agent editorial room. Every level includes a `--dry-run` path that works without provider credentials.

## When It Fits

DeepStrike fits applications where Agents need durable sessions, controlled tools, memory, delegation, dynamic workflows, or consistent behavior across Node.js, Python, Rust, and WASM.

For a stateless chat endpoint or a one-off prompt with no tools, a provider SDK may be enough. Add DeepStrike when the Agent needs to keep working, coordinate other Agents, respect limits, or explain and recover its own execution.

## Current Scope

The current focus is a reliable local Agent Process Runtime. It supports durable process trees, generalized wait and wake, hierarchical budgets, capability attenuation, local IPC, process supervision, deterministic scheduling, checkpoint/replay recovery, and host-provided integrations such as remote tools or MCP servers.

The framework does not currently promise remote worker leasing, task migration, distributed takeover, or a distributed message broker. External effects are at-least-once, so applications should use idempotency keys and reconciliation for databases, mail, payments, and similar systems.

Billing, pricing, taxes, and tenant accounting belong to the application using DeepStrike. The framework exposes usage information and runtime events, but does not define product billing policy.

## Documentation

| You want to... | Start here |
| --- | --- |
| Understand the runtime model | [Agent Process Runtime](./docs/en/architecture/agent-process-runtime.md) |
| Learn the Agent model | [Getting Started](./docs/en/getting-started/index.md) |
| Choose an API | [runAgent vs RuntimeRunner](./docs/en/getting-started/run-agent-vs-runner.md) |
| Build workflows | [Dynamic Workflows](./docs/en/guides/workflow.md) |
| Add tools and integrations | [Execution Plane & Tools](./docs/en/guides/execution-plane-and-tools.md) and [Provider Routing](./docs/en/guides/provider-routing.md) |
| Add governance | [Governance](./docs/en/guides/governance.md) |
| Add skills and memory | [Skills](./docs/en/guides/skills.md) and [Memory](./docs/en/guides/memory.md) |
| Build recoverable runs | [Session, Replay & Recovery](./docs/en/guides/session-replay-and-recovery.md) |
| Inspect the full API | [Reference](./docs/en/reference/index.md) |

## Repository Layout

```text
crates/deepstrike-core/   Shared runtime implementation
crates/deepstrike-node/   Node.js native bindings
crates/deepstrike-py/     Python native bindings
crates/deepstrike-wasm/   WASM bindings
node/                     TypeScript SDK
python/                   Python SDK
rust/                     Rust SDK
wasm/                     Browser and edge SDK
example/                  Runnable Agent curriculum
docs/                     VitePress documentation source
tests/                    Cross-language contracts and fixtures
```

## Development

Requirements: Rust 1.85+, Node.js 18+, and Python 3.10+.

```bash
cargo test
npm run docs:build
```

For SDK-specific commands, use the language README linked above. Before opening a pull request, read [CONTRIBUTING.md](./CONTRIBUTING.md). Report vulnerabilities through [SECURITY.md](./SECURITY.md).

## License

DeepStrike is released under the [MIT License](./LICENSE). It is an independent open-source project and is not affiliated with or endorsed by any model provider.
