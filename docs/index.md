# DeepStrike Documentation

DeepStrike is a cross-language agent runtime built around a pure-Rust kernel. The kernel handles all decision logic — loop control, context compression, governance, signal routing — while language SDKs handle I/O: LLM calls, tool execution, file access, and storage.

---

## Guides

| Guide | Description |
| --- | --- |
| [Quick Start](./quick-start.md) | Install, run your first agent, stream output, add tools, use images |
| [Architecture](./architecture.md) | Layer overview (0–4), kernel design, SDK layer, binding architecture |
| [Core Concepts](./core-concepts.md) | Skills, Memory, Knowledge, Harness, Signals, Collaboration, Safety |
| [Context Slots & Compression](./context-partition-compression.md) | **Current:** four-slot model, compression tiers, renderer, renewal |
| [Collaboration](./collaboration.md) | VerificationContract, AgentPool, ContractDrivenHarness, Modes, HandoffBus |
| [SDK Kernel Driver Parity](./sdk-kernel-driver-parity.md) | Cross-SDK plan for aligning Node, Python, Rust, and WASM around the kernel-driver contract |
| [Providers](./providers.md) | All LLM providers, RenderedContext slots, Anthropic prompt caching, multimodal |
| [Release Runbook](./release-runbook.md) | Version propagation, verification, release flow, and recovery |

## SDK guides

| Guide | Description |
| --- | --- |
| [sdk-guide-nodejs.md](./sdk-guide-nodejs.md) | Node.js SDK API 使用指南 |
| [sdk-guide-python.md](./sdk-guide-python.md) | Python SDK API 使用指南 |
| [sdk-guide-rust.md](./sdk-guide-rust.md) | Rust SDK API 使用指南 |

## Specifications

| Spec | Description |
| --- | --- |
| [spec-kernel-abi.md](./spec-kernel-abi.md) | `KernelInput` / `KernelAction` / `KernelObservation` JSON ABI |
| [spec-context-optimization-v3.md](./spec-context-optimization-v3.md) | P0/P1 context performance (token counting, prompt caching, renderer) |
| [spec-context-compression-v2.md](./spec-context-compression-v2.md) | *(superseded)* six-partition v2 design |
| [implementation-agent-os-kernel.md](./implementation-agent-os-kernel.md) | Agent OS kernel roadmap and phase gates |

See also [CHANGELOG.md](../CHANGELOG.md).

---

## Packages

| Package | Language | Install |
| --- | --- | --- |
| `@deepstrike/sdk` | TypeScript / Node.js | `npm install @deepstrike/sdk` |
| `deepstrike` | Python | `pip install deepstrike` |
| `deepstrike-sdk` | Rust | `cargo add deepstrike-sdk` |
| `@deepstrike/wasm` | TypeScript / Browser | `npm install @deepstrike/wasm` |

Package READMEs: [node/README.md](../node/README.md) · [python/README.md](../python/README.md) · [rust/README.md](../rust/README.md) · [wasm/README.md](../wasm/README.md)

---

## API Reference

- **Node.js** — `node/src/` — TypeScript source with JSDoc
- **Python** — `python/deepstrike/` — type-annotated Python source
- **Rust** — run `cargo doc --open -p deepstrike-sdk` for rendered crate docs
- **Kernel internals** — `crates/deepstrike-core/` — Rust source

---

## Stream event reference

Every SDK yields the same typed event stream from `RuntimeRunner.run()` / `run_streaming()`:

| Event | Key fields | Description |
| --- | --- | --- |
| `text_delta` | `delta: string` | Incremental text from the model |
| `thinking_delta` | `delta: string` | Incremental reasoning/thinking trace |
| `tool_call` | `id`, `name`, `arguments` | Model requested a tool call |
| `tool_result` | `call_id`, `name`, `content`, `is_error` | Tool execution result |
| `permission_request` | `call_id`, `tool_name`, `arguments`, `reason` | Governance blocked; awaiting user approval |
| `done` | `iterations`, `total_tokens`, `status` | Session complete |
| `error` | `message: string` | Unrecoverable error |

**`done.status` values:** `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `milestone_pending` · `error`

---

## Building from source

```bash
# Rust kernel + all native bindings
cargo build

# Node.js SDK
cd node && npm install && npm run build

# Python SDK  (requires maturin)
cd python && maturin develop

# WASM SDK
cd wasm && npm install && npm run build

# Tests
cargo test
cd node && npm test
cd python && pytest
```

**Requirements:** Rust 1.85+ · Node.js 18+ · Python 3.10+

---

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md).

## License

Apache-2.0 OR MIT
