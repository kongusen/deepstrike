# DeepStrike

<p align="center">
  <strong>Agent OS microkernel for cross-language agent runtimes.</strong>
</p>

<p align="center">
  <a href="https://github.com/kongusen/deepstrike/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kongusen/deepstrike?sort=semver"></a>
  <a href="https://www.npmjs.com/package/@deepstrike/sdk"><img alt="npm" src="https://img.shields.io/npm/v/@deepstrike/sdk?label=npm"></a>
  <a href="https://pypi.org/project/deepstrike/"><img alt="PyPI" src="https://img.shields.io/pypi/v/deepstrike?label=pypi"></a>
  <a href="https://crates.io/crates/deepstrike-sdk"><img alt="crates.io" src="https://img.shields.io/crates/v/deepstrike-sdk?label=crates.io"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://discord.gg/cwS3RBYCv"><img alt="Discord" src="https://img.shields.io/badge/discord-join-5865F2?logo=discord&logoColor=white"></a>
</p>

<p align="center">
  <a href="./docs/getting-started/quick-start.md">Quick Start</a>
  · <a href="./docs/index.md">Docs</a>
  · <a href="./docs/architecture/">Architecture</a>
  · <a href="./docs/guides/providers.md">Providers</a>
  · <a href="./CHANGELOG.md">Changelog</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

DeepStrike is a runtime kernel and SDK family for building AI agents that need replayable state, governed tools, context compression, provider portability, and host-language control. The Rust kernel owns the agent semantics; the SDKs own effects such as LLM calls, tools, storage, processes, network, and UI.

```text
┌──────────────────────────────────────────────────────────────────────┐
│ Host SDKs: Node.js · Python · Rust · WASM                            │
│ Providers · tools · permissions · storage · signals · orchestration  │
└───────────────────────────────┬──────────────────────────────────────┘
                                │ KernelInput / KernelAction (JSON ABI)
                    ┌───────────▼───────────┐
                    │   deepstrike-core     │
                    │   pure Rust, zero I/O │
                    │   state machine       │
                    │   context VM          │
                    │   capability bus      │
                    │   security pipeline   │
                    │   transactions        │
                    │   milestones          │
                    └───────────────────────┘
```

## Why DeepStrike

- **Kernel-owned agent semantics**: loop control, context layout, rollback, milestones, signals, and audit behavior live behind one ABI.
- **Host-owned effects**: SDKs perform all I/O, so runtime behavior is portable across Node.js, Python, Rust, and WASM.
- **Provider portability**: Anthropic, OpenAI, Qwen, DeepSeek, MiniMax, Kimi, Ollama, and OpenAI-compatible gateways share one event stream.
- **Governed execution**: every tool call flows through capability checks, constraints, permission gates, vetoes, rate limits, sandbox policy, and audit logging.
- **Long-run context control**: the kernel uses a four-slot context model and compresses history only, preserving stable system and knowledge blocks.
- **Multi-agent contracts**: sub-agents, milestone gates, verifier harnesses, and handoff artifacts are runtime primitives instead of prompt conventions.

## Packages

| Package | Runtime | Install |
| --- | --- | --- |
| `@deepstrike/sdk` | Node.js / TypeScript | `npm install @deepstrike/sdk` |
| `deepstrike` | Python | `pip install deepstrike` |
| `deepstrike-sdk` | Rust | `cargo add deepstrike-sdk` |
| `@deepstrike/wasm` | Browser / edge / WASM | `npm install @deepstrike/wasm` |

Current workspace version: **0.2.4**.

## Quick Start

### Node.js

```bash
npm install @deepstrike/sdk
```

```typescript
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

```python
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
deepstrike-sdk = "0.2.4"
```

See the full language guides for complete setup, streaming, tools, images, governance, memory, and provider examples.

## Core Concepts

| Concept | What it does | Read more |
| --- | --- | --- |
| Kernel ABI | Versioned `KernelInput`, `KernelAction`, and `KernelObservation` contract used by every SDK | [Kernel ABI](./docs/reference/kernel-abi.md) |
| Context VM | Four LLM API slots: stable system, knowledge, live task state, and compressible history | [Context Slots](./docs/concepts/context-slots-compression.md) |
| ExecutionPlane | Host-side tool registry and dispatcher | [Quick Start](./docs/getting-started/quick-start.md) |
| Governance | Capability, permission, constraint, veto, sandbox, and audit pipeline for tool calls | [Core Concepts](./docs/concepts/core-concepts.md) |
| Providers | Shared provider abstraction and typed event stream | [Providers](./docs/guides/providers.md) |
| Collaboration | Verifier contracts, harnesses, agent pools, handoffs, and sub-agent isolation | [Collaboration](./docs/guides/collaboration.md) |

## Documentation

| Start here | Use when you need |
| --- | --- |
| [Documentation hub](./docs/index.md) | The full map of guides, references, and runbooks |
| [Getting Started](./docs/getting-started/) | Install and run your first agent |
| [Guides](./docs/guides/) | SDK, provider, and collaboration workflows |
| [Concepts](./docs/concepts/) | Runtime mental model and core terminology |
| [Architecture](./docs/architecture/) | Kernel, SDK, binding, and runtime-loop design |
| [Reference](./docs/reference/) | ABI and lifecycle contracts |
| [Operations](./docs/operations/) | Versioning and release workflows |

Package-specific READMEs are available in [`node/`](./node/README.md), [`python/`](./python/README.md), [`rust/`](./rust/README.md), and [`wasm/`](./wasm/README.md).

## Repository Layout

```text
crates/deepstrike-core/   Pure Rust kernel
crates/deepstrike-node/   Node.js native binding
crates/deepstrike-py/     Python native binding
crates/deepstrike-wasm/   WASM binding
node/                     TypeScript host SDK
python/                   Python host SDK
rust/                     Rust host SDK
wasm/                     Browser / edge SDK
docs/                     Organized documentation system
docs/getting-started/     Install and first-run material
docs/guides/              SDK, provider, and collaboration guides
docs/concepts/            Runtime concepts and context model
docs/architecture/        Kernel and SDK architecture
docs/reference/           ABI and lifecycle reference
docs/operations/          Release and maintenance runbooks
tests/                    Cross-language SDK tests
scripts/                  Release, smoke, and verification scripts
```

## Development

Requirements: Rust 1.85+, Node.js 18+, Python 3.10+.

```bash
# Rust workspace
cargo build
cargo test

# Node.js SDK
cd node
npm install
npm run build
npm test

# Python SDK
cd python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-asyncio
maturin develop --release
pytest

# WASM SDK
cd wasm
npm install
npm run build
npm test
```

## Community and Support

- Join the community on [Discord](https://discord.gg/cwS3RBYCv).
- Open bugs and feature requests in [GitHub Issues](https://github.com/kongusen/deepstrike/issues).
- Read [CONTRIBUTING.md](./CONTRIBUTING.md) before sending a pull request.
- Report security issues using [SECURITY.md](./SECURITY.md).

## License

DeepStrike is released under the [MIT License](./LICENSE).
