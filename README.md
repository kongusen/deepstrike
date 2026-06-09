<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>Agent OS microkernel for replayable, governed, cross-language AI agent runtimes.</strong>
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
  <a href="./docs/index.md">Documentation</a>
  · <a href="./docs/getting-started/quick-start.md">Quick Start</a>
  · <a href="./docs/guides/index.md">SDK Guides</a>
  · <a href="./docs/architecture/index.md">Architecture</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

---

DeepStrike is a runtime kernel and SDK family for building AI agents that need durable state, governed tools, provider portability, long-context control, and host-language ownership of side effects.

The core kernel is a pure Rust state machine. Host SDKs for Node.js, Python, Rust, and WASM own provider calls, tools, storage, processes, network, UI, and deployment concerns.

```text
Host SDKs: Node.js / TypeScript · Python · Rust · WASM
Providers · tools · permissions · storage · signals · orchestration
                               │
                               ▼
                    KernelInput / KernelAction
                               │
                               ▼
deepstrike-core: pure Rust kernel · zero I/O · replayable state machine
Context VM · governance pipeline · capability bus · transactions · milestones
```

**0.2.6 — Agent OS consolidation.** M1 scheduler authority, M2 resource quotas, M3 handle residency and read-time projection, plus cross-SDK native profile and memory policy wiring. See [CHANGELOG 0.2.6](./CHANGELOG.md#026---2026-06-03).

## What Agent OS Gives You

These are not internal refactors — they change what you can build without custom runner glue in every host SDK.

| Before (≤ 0.2.4) | After (0.2.6) |
| :--- | :--- |
| Scheduling, compression, and permission logic scattered per SDK | Unified syscall trap, TCB lifecycle, and MM eviction funnel — same semantics in Node, Python, and Rust |
| Large tool outputs and long sessions hit token walls | Layer-1 spool (preview + `.spool/` ref) and semantic page-out → long-term memory |
| Governance and signal routing were optional SDK plugins | OS native profile: declarative governance and in-kernel signal routing on by default |
| Long-term memory mostly via meta-tools and idle pipelines | `writeMemory` / `queryMemory` kernel syscalls with validation and audit events |
| Session logs skewed toward chat + tools | Full OS event stream and rebuildable OS snapshots |
| Quotas and memory rules fixed at compile time | `set_resource_quota` + `set_memory_policy` enforced at the syscall trap, opt-in at runtime |
| Config surface drifted per SDK | Same 8 config-in options across Node, Python, Rust, and WASM (M1/M2/M3 consolidation) |

**Kernel-mediated runtime (M0–M4)** — Tool calls, spawns, compression, and signals pass through one kernel gate with an explicit lifecycle (Ready / Running / Blocked / Suspended). You implement I/O; the kernel decides *when* and *whether*. `wake(sessionId)` and cross-language tooling see consistent behavior.

**Longer, sturdier sessions** — Oversized tool results stay in context as a preview plus a spool reference; the model reads the full payload on demand. Semantic page-out archives summaries into long-term memory and satisfies page-in requests on the way back in.

**Safety and governance by default** — Every run loads declarative governance (deny / ask_user / rate-limit / param rules) and in-kernel signal disposition (Interrupt / Queue / Observe / Dropped). Policy, not ad-hoc handler checks.

**Long-term memory as syscalls (Phase-7)** — Write and query memory outside the main tool loop: kernel validation before commit, search → selection → retrieval closure. Failed writes are auditable; good memory is durable without polluting history.

**Multi-agent and multi-signal orchestration** — Sub-agents register in the kernel process table; parent runs suspend until join. External signals compose with the main loop instead of racing it.

**Observable like an OS log** — Spool, page-out, signals, processes, budgets, and memory events land in session logs with categories (`syscall` · `sched` · `mm` · `proc` · `ipc`). Rebuild OS snapshots from one event stream; replay strips audit events when reconstructing LLM messages.

| You need… | Mechanism |
| :--- | :--- |
| Policy before tools run | Declarative governance policy (default: allow-all native profile) |
| External interrupts | Signal source + in-kernel attention policy |
| Huge tool output | Layer-1 spool; host SDK writes `.spool/` refs |
| Durable recall across runs | Long-term memory store + semantic page-out summarization |
| Programmatic memory I/O | Kernel `writeMemory` / `queryMemory` syscalls |
| Debug / compliance | Session log events + OS snapshot rebuild |

SDK-specific APIs and examples: [Node.js](./node/README.md#what-agent-os-gives-you) · [Python](./python/README.md#what-agent-os-gives-you) · [Rust](./docs/guides/sdk-rust.md)

## Documentation System

DeepStrike's documentation is organized as a modern VitePress reading system with a stable project header, navigation by reader intent, SDK-specific guides, and community entry points.

| Reader path | Start here |
| :--- | :--- |
| New users | [Quick Start](./docs/getting-started/quick-start.md) |
| SDK users | [Node.js](./docs/guides/sdk-nodejs.md), [Python](./docs/guides/sdk-python.md), [Rust](./docs/guides/sdk-rust.md), [WASM](./docs/guides/index.md) |
| Runtime designers | [Agent OS](./docs/concepts/agent-os.md) · [Core Concepts](./docs/concepts/core-concepts.md) |
| Architecture reviewers | [Architecture Overview](./docs/architecture/overview.md) |
| Integrators | [Provider Guide](./docs/guides/providers.md) and [Kernel ABI](./docs/reference/kernel-abi.md) |
| Operators | [Release Runbook](./docs/operations/release-runbook.md) |
| Contributors | [Contributing Guide](./CONTRIBUTING.md) |

Run the local docs site:

```bash
npm install
npm run docs:dev
```

Build the static docs:

```bash
npm run docs:build
```

## Language And Runtime Support

| Runtime | Package | Install |
| :--- | :--- | :--- |
| Node.js / TypeScript | `@deepstrike/sdk` | `npm install @deepstrike/sdk` |
| Python | `deepstrike` | `pip install deepstrike` |
| Rust | `deepstrike-sdk` | `cargo add deepstrike-sdk` |
| Browser / Edge / WASM | `@deepstrike/wasm` | `npm install @deepstrike/wasm` |

Current workspace version: `0.2.6`.

## Quick Start

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
  properties: {
    x: { type: "number" },
    y: { type: "number" },
  },
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
deepstrike-sdk = "0.2.8"
```

See the [SDK guides](./docs/guides/index.md) for full examples, provider configuration, streaming events, governance hooks, and collaboration patterns.

## Core Capabilities

- **Agent OS runtime (0.2.6+)**: kernel-mediated syscall trap, scheduler lifecycle, memory paging, process table, resource quotas, and IPC — host SDKs own all side effects.
- **Replayable kernel semantics**: loop control, context layout, rollback, milestones, signals, and audit behavior live behind a versioned ABI.
- **Host-owned effects**: SDKs handle I/O, providers, tools, persistence, processes, and network boundaries.
- **Provider portability**: Anthropic, OpenAI, Qwen, DeepSeek, MiniMax, Kimi, Ollama, and OpenAI-compatible gateways share a unified event stream.
- **Governed execution**: tool calls flow through in-kernel policy, capability checks, permission gates, vetoes, rate limits, and audit logging.
- **Long-run context control**: four-slot Context VM, Layer-1 large-result spool, semantic page-out, and compression funnel for durable long sessions.
- **Memory syscalls**: validated long-term write/query outside the tool loop, with session-log and OS snapshot counters.
- **Collaboration primitives**: sub-agents, milestone gates, verifier harnesses, and handoff artifacts are runtime primitives.

## Repository Layout

```text
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

Requirements:

- Rust 1.85+
- Node.js 18+
- Python 3.10+

```bash
cargo build
cargo test
```

```bash
cd node
npm install
npm run build
npm test
```

```bash
cd python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-asyncio
maturin develop --release
pytest
```

```bash
cd wasm
npm install
npm run build
npm test
```

## Community

- Join the developer community on [Discord](https://discord.gg/cwS3RBYCv).
- Report issues or request features in [GitHub Issues](https://github.com/kongusen/deepstrike/issues).
- Read [CONTRIBUTING.md](./CONTRIBUTING.md) before opening a pull request.
- Report security issues through the process in [SECURITY.md](./SECURITY.md).

## License

DeepStrike is released under the [MIT License](./LICENSE).
