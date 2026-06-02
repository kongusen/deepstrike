# Changelog

All notable changes to DeepStrike are documented here.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.5] - 2026-06-02

Agent OS release: kernel three-primitives refactor (M0–M4), OS native profile defaults, Layer-1 large-result spool, semantic page-out pipeline, and Phase-7 memory syscalls — across core, Node, Python, Rust, and Wasm event mapping.

### Added

#### Core — Agent OS primitives (M0–M4)

- **M0 — Three primitives lens:** Kernel responsibilities reorganized around syscall trap, TCB (turn control block), and MM (memory management) modules.
- **M1 — Turn lifecycle:** `LoopPhase` split into explicit turn-steps; root TCB owns run lifecycle (Ready / Running / Blocked / Suspended / Terminated).
- **M2 — Unified syscall trap:** Tool calls and `spawn_sub_agent` route through a single kernel gate before SDK execution.
- **M3 — Unified eviction funnel:** `plan_eviction` consolidates compression / page-out decisions into one checkpoint.
- **M4 — Kernel event log:** Observations tagged with OS categories (`syscall` · `sched` · `mm` · `proc` · `ipc`); replay and repair paths ignore OS audit events when reconstructing LLM messages.

#### Core — Layer 1 large-result spool

- Kernel emits `large_result_spooled` when a single tool result exceeds the size threshold; context keeps a short preview plus a spool reference.
- New `SessionEvent::LargeResultSpooled` for session-log and replay accounting.

#### Core — In-kernel signal router (default)

- **M4 COMPAT removal:** In-kernel `SignalRouter` is now the default path; legacy SDK-side disposition routing is dropped.
- `SetAttentionPolicy` configures queue capacity; `SignalDisposed` observations record disposition and queue depth.

#### Core — Phase-7 memory syscalls

- New `mm/memory.rs`: `MemoryKind` (User / Feedback / Project / Reference), `MemoryMetadata`, `MemoryValidation`, and `validate_memory_write` (forbidden-pattern and size rules).
- Kernel ABI: `SetMemoryPolicy`, `WriteMemory`, `QueryMemory`; observations `MemoryWritten`, `MemoryValidationFailed`, `MemoryQueried`.
- `SessionEvent::MemoryValidationFailed`; `KernelInputEvent::MemoryRetrievalResult` closes the query loop after SDK memory selection.
- Event-log / replay counters: `memory_written_count`, `memory_queried_count`, `memory_validation_failed_count`, `memory_retrieval_result_count`.

#### SDK — OS native profile (Node reference; Python / Rust parity)

- **Defaults on every run:** `governancePolicy` (`DEFAULT_NATIVE_GOVERNANCE_POLICY`) and `attentionPolicy` (`DEFAULT_NATIVE_ATTENTION_POLICY`, queue size 64) loaded into the kernel before `start_run`.
- Declarative governance (deny / ask_user / rate-limit / param rules) enforced in-kernel before tool execution.
- `RuntimeOptions.attentionPolicy`, `RuntimeOptions.governancePolicy`, `RuntimeOptions.dreamSummarizer`, `RuntimeOptions.resultSpool` (Node); equivalent options in Python and Rust runners.

#### SDK — Layer 1 spool I/O (S1)

- **Node / Python / Rust:** SDK writes full oversized tool payloads to `.spool/` (SHA-256 keyed files under cwd); session log records `spool_ref`.
- `LocalExecutionPlane` (Node) transparently resolves `read_file` paths under `.spool/`.
- Cross-SDK spool parity tests and session-log event mapping.

#### SDK — Semantic page-out → DreamStore (S2)

- On kernel `page_out { tier_hint: "semantic" }`, SDK summarizes archived content via `dreamSummarizer` / `dreamProvider` and commits to `DreamStore`.
- `page_in_requested` satisfied from `DreamStore`, `KnowledgeSource`, and a local semantic page-out cache before feeding `page_in` back to the kernel.
- Layer-5 AutoCompact → semantic page-out contract pinned in core tests.

#### SDK — Phase-7 memory syscalls (Node / Python / Rust)

- **`writeMemory` / `write_memory`:** Kernel `WriteMemory` validation → `DreamStore.commit()` on success; `memory_validation_failed` on reject.
- **`queryMemory` / `query_memory`:** Kernel `QueryMemory` → `DreamStore.search()` → `selectMemories` (Node `memory/agent.ts`; new Python `deepstrike/memory/agent.py`) → `memory_retrieval_result` fed back to the kernel.
- Session events: `memory_written`, `memory_queried`, `memory_validation_failed`, `memory_retrieval_result`.
- **Wasm:** Session-event type mapping only (no runner-level `writeMemory` / `queryMemory` API yet).

#### SDK — Observability and OS snapshot

- Unified `kernelObservationToSessionEvent` / `appendObservations` pipeline for spool, page-out, signals, process, budget, and memory events.
- OS snapshot rebuild (Node / Python): `pageOutCount`, `spoolCount`, signal and process tables, memory event counters (`memory_retrieval_result` counted separately from category-tagged kernel kinds).
- `scripts/check-sdk-parity.mjs`: memory syscall surface markers.

#### Tests

- `node/tests/runtime/memory-syscall.test.ts`, `python/tests/test_memory_syscall.py`, Rust runner memory syscall and validation coverage; session-log and OS snapshot regression tests across SDKs.

### Changed

- **Breaking (behavioral):** New runs use the in-kernel signal router and native governance profile by default; SDKs that relied on legacy signal disposition or implicit allow-all governance should set explicit policies or opt out via configuration.
- **Node / Python READMEs:** Major expansion — architecture, four-slot context model, OS native profile options, Layer-1 spool, semantic page-out, governance, signals, sub-agents, and Phase-7 memory syscalls.
- **Root README and docs:** Version references updated to `0.2.5`; quick-start and index aligned with Agent OS terminology.
- **Python `session_log`:** Extended event kinds and category tagging for kernel OS events (parity with Node).

### Notes

- Rebuild Node native bindings after upgrade: `cd crates/deepstrike-node && napi build --platform`.
- Python full ABI for `memory_retrieval_result` requires a fresh `maturin develop`; older bindings degrade gracefully via try/except in the kernel step path.

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
- **Documentation:** Four-slot context model across README, guides, providers, WASM/Python/Node/Rust package READMEs, and [docs/concepts/context-slots-compression.md](./docs/concepts/context-slots-compression.md).

### Changed

- **Context architecture:** Six-partition narrative replaced by four LLM API slots (`system_stable`, `system_knowledge`, State turn, `history`). Compression summaries route through `task_state.compression_log` → Slot 3.
- **Memory preload:** `initialMemory` / `initial_memory` / `add_knowledge_message` → Slot 2 (`system_knowledge`); meta-tool retrieval still lands in history.

### Removed

- **Python SDK:** `RuntimeRunner.push_artifact()` — kernel no longer handles `push_artifact` events after four-slot refactor. Use `initial_memory` for durable preload or rely on history compression tiers for large in-run outputs.
- **Rust SDK:** `RuntimeRunner::push_artifact()` — removed for the same reason. Use `initial_memory` → Slot 2 or history compression tiers.
- **Rust SDK:** `KernelInputEvent::AddMemoryMessage` call site updated to `AddKnowledgeMessage` for `initial_memory` preload.

### Deprecated

- **`push_artifact` ABI event** — fixture retained for compatibility tests only; not processed by current kernel.
- **Context compression v2 design notes** — superseded by four-slot documentation and moved out of the public docs set.
