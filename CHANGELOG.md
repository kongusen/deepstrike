# Changelog

All notable changes to DeepStrike are documented here.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.4] - 2026-05-29

### Fixed

- **Node SDK:** `DeepSeekProvider.stream()` now requests `stream_options.include_usage` and emits `usage` events — fixes token accounting and compression pressure (`rho`) when using DeepSeek.
- **E2E harness:** Correct kernel-turn ↔ LLM-turn correlation for post-compression State turn snapshots; record metrics even when the provider stream throws.

### Changed

- **E2E scenarios (K01/K03):** Relaxed rho validation for batched tool calls; K03 uses sequential fill pressure and multi-path compression_log checks.

## [0.2.3] - 2026-05-28

### Added

- **Python SDK:** `RuntimeOptions.sub_agent_harness` — spawned sub-agents run through `HarnessLoop` + `EvalPipeline`, with criteria from `AgentRunSpec.milestones.phases[].criteria` (parity with Node `subAgentHarness`).
- **Python SDK:** `SubAgentHarnessConfig` exported from `deepstrike`.
- **Documentation:** Four-slot context model across README, guides, providers, WASM/Python/Node/Rust package READMEs, and [docs/context-partition-compression.md](docs/context-partition-compression.md).

### Changed

- **Context architecture:** Six-partition narrative replaced by four LLM API slots (`system_stable`, `system_knowledge`, State turn, `history`). Compression summaries route through `task_state.compression_log` → Slot 3.
- **Memory preload:** `initialMemory` / `initial_memory` / `add_knowledge_message` → Slot 2 (`system_knowledge`); meta-tool retrieval still lands in history.

### Removed

- **Python SDK:** `RuntimeRunner.push_artifact()` — kernel no longer handles `push_artifact` events after four-slot refactor. Use `initial_memory` for durable preload or rely on history compression tiers for large in-run outputs.
- **Rust SDK:** `RuntimeRunner::push_artifact()` — removed for the same reason. Use `initial_memory` → Slot 2 or history compression tiers.
- **Rust SDK:** `KernelInputEvent::AddMemoryMessage` call site updated to `AddKnowledgeMessage` for `initial_memory` preload.

### Deprecated

- **`push_artifact` ABI event** — fixture retained for compatibility tests only; not processed by current kernel.
- **`docs/spec-context-compression-v2.md`** — superseded by four-slot documentation.
