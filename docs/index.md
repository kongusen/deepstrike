# DeepStrike Documentation

DeepStrike documentation is organized by user intent: start quickly, learn the model, build with an SDK, understand internals, then use references and runbooks when you need exact contracts.

## Start Here

| Path | Use for |
| --- | --- |
| [Getting Started](./getting-started/) | Install packages and run your first agent |
| [Guides](./guides/) | Build agents with SDKs, providers, tools, and collaboration APIs |
| [Concepts](./concepts/) | Understand the runtime vocabulary and mental model |
| [Architecture](./architecture/) | Read about the kernel / host split and runtime design |
| [Reference](./reference/) | Look up stable runtime and ABI contracts |
| [Operations](./operations/) | Release, verification, and publishing workflows |

## Recommended Reading Paths

### I want to run an agent

1. [Quick Start](./getting-started/quick-start.md)
2. [Providers](./guides/providers.md)
3. [Node.js SDK](./guides/sdk-nodejs.md), [Python SDK](./guides/sdk-python.md), or [Rust SDK](./guides/sdk-rust.md)

### I want to understand the runtime

1. [Core Concepts](./concepts/core-concepts.md)
2. [Context Slots and Compression](./concepts/context-slots-compression.md)
3. [Architecture Overview](./architecture/overview.md)
4. [Kernel ABI Reference](./reference/kernel-abi.md)

### I want to contribute

1. [CONTRIBUTING.md](../CONTRIBUTING.md)
2. [Architecture Overview](./architecture/overview.md)
3. [Kernel ABI Reference](./reference/kernel-abi.md)
4. [Release Runbook](./operations/release-runbook.md)

## SDKs and Packages

| SDK | Package | Guide |
| --- | --- | --- |
| Node.js / TypeScript | `@deepstrike/sdk` | [guides/sdk-nodejs.md](./guides/sdk-nodejs.md) |
| Python | `deepstrike` | [guides/sdk-python.md](./guides/sdk-python.md) |
| Rust | `deepstrike-sdk` | [guides/sdk-rust.md](./guides/sdk-rust.md) |
| WASM / browser / edge | `@deepstrike/wasm` | [wasm/README.md](../wasm/README.md) |

Package READMEs: [node](../node/README.md) · [python](../python/README.md) · [rust](../rust/README.md) · [wasm](../wasm/README.md)

## API Surface

All SDKs expose the same runtime shape:

- `RuntimeRunner` starts, resumes, and streams sessions.
- `ExecutionPlane` registers and executes host tools.
- `SessionLog` records replayable runtime history.
- `LLMProvider` streams model output into a shared event model.
- `Governance` configures permissions and tool-call policy.

The public event stream includes `text_delta`, `thinking_delta`, `tool_call`, `tool_result`, `permission_request`, `done`, and `error`. See [Providers](./guides/providers.md) and the SDK guides for language-specific examples.

## Build From Source

Requirements: Rust 1.85+, Node.js 18+, Python 3.10+.

```bash
cargo build
cargo test

cd node
npm install
npm run build
npm test

cd ../python
python3 -m venv .venv
source .venv/bin/activate
pip install maturin pytest pytest-asyncio
maturin develop --release
pytest

cd ../wasm
npm install
npm run build
npm test
```

## Community

- Discord: <https://discord.gg/cwS3RBYCv>
- Issues: <https://github.com/kongusen/deepstrike/issues>
- Releases: <https://github.com/kongusen/deepstrike/releases>
