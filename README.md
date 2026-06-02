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

## Documentation System

DeepStrike's documentation is organized as a modern VitePress reading system with a stable project header, navigation by reader intent, SDK-specific guides, and community entry points.

| Reader path | Start here |
| :--- | :--- |
| New users | [Quick Start](./docs/getting-started/quick-start.md) |
| SDK users | [Node.js](./docs/guides/sdk-nodejs.md), [Python](./docs/guides/sdk-python.md), [Rust](./docs/guides/sdk-rust.md), [WASM](./docs/guides/index.md) |
| Runtime designers | [Core Concepts](./docs/concepts/core-concepts.md) |
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

Current workspace version: `0.2.4`.

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
deepstrike-sdk = "0.2.4"
```

See the [SDK guides](./docs/guides/index.md) for full examples, provider configuration, streaming events, governance hooks, and collaboration patterns.

## Core Capabilities

- **Replayable kernel semantics**: loop control, context layout, rollback, milestones, signals, and audit behavior live behind a versioned ABI.
- **Host-owned effects**: SDKs handle I/O, providers, tools, persistence, processes, and network boundaries.
- **Provider portability**: Anthropic, OpenAI, Qwen, DeepSeek, MiniMax, Kimi, Ollama, and OpenAI-compatible gateways share a unified event stream.
- **Governed execution**: tool calls flow through capability checks, constraints, permission gates, vetoes, rate limits, sandbox policy, and audit logging.
- **Long-run context control**: a four-slot Context VM compresses history while preserving stable system and knowledge blocks.
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
