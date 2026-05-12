# DeepStrike Documentation

DeepStrike is a cross-language agent runtime built around a pure-Rust kernel. The kernel handles all decision logic — loop control, context compression, governance, signal routing — while language SDKs handle I/O: LLM calls, tool execution, file access, and storage.

---

## Guides

| Guide | Description |
|-------|-------------|
| [Quick Start](./quick-start.md) | Install, run your first agent, stream output, add tools, use images |
| [Architecture](./architecture.md) | Kernel design, SDK layer, binding architecture, message content model |
| [Core Concepts](./core-concepts.md) | Skills, Memory, Knowledge, Harness, Signals, Safety |
| [Providers](./providers.md) | All LLM providers, configuration, thinking/reasoning flags, multimodal support |

---

## Packages

| Package | Language | Install |
|---------|----------|---------|
| `@deepstrike/sdk` | TypeScript / Node.js | `npm install @deepstrike/sdk` |
| `deepstrike` | Python | `pip install deepstrike` |
| `deepstrike-sdk` | Rust | `cargo add deepstrike-sdk` |
| `@deepstrike/wasm` | TypeScript / Browser | `npm install @deepstrike/wasm` |

---

## API Reference

- **Node.js** — `node/src/` — TypeScript source with JSDoc
- **Python** — `python/deepstrike/` — type-annotated Python source
- **Rust** — run `cargo doc --open -p deepstrike-sdk` for rendered crate docs
- **Kernel internals** — `crates/deepstrike-core/` — Rust source

---

## Stream event reference

Every SDK yields the same typed event stream from `agent.run_streaming()`:

| Event | Key fields | Description |
|-------|-----------|-------------|
| `text_delta` | `delta: string` | Incremental text from the model |
| `thinking_delta` | `delta: string` | Incremental reasoning/thinking trace |
| `tool_call` | `id`, `name`, `arguments` | Model requested a tool call |
| `tool_result` | `call_id`, `name`, `content`, `is_error` | Tool execution result |
| `permission_request` | `call_id`, `tool_name`, `arguments`, `reason` | Governance blocked; awaiting user approval |
| `done` | `iterations`, `total_tokens`, `status` | Session complete |
| `error` | `message: string` | Unrecoverable error |

**`done.status` values:** `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`

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
