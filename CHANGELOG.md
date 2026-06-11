# Changelog

All notable changes to DeepStrike are documented here.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

Dynamic workflows: the kernel can now author and run agent-orchestration DAGs as a first-class primitive — every node spawn passes the syscall gate, so quotas, trust, and future spawn policies apply per node for free. Inspired by Anthropic's *A harness for every task*.

### What this enables

| Before | After |
|---|---|
| SDK orchestrates sub-agents; kernel adjudicates one spawn at a time | Kernel owns a workflow **DAG**, spawning ready nodes as **gated batches** and advancing on completions (`load_workflow` ABI) |
| No comparative-judgment or unbounded-loop control in-kernel | Dynamic control-flow **node kinds** on the one workflow executor: **`Loop`** (until-done), **`Classify`** (conditional branch), **`Tournament`** (pairwise bracket) |
| Workflow shapes hand-built each time | **Templates**: `fanout_synthesize`, `generate_and_filter`, `verify_rules`, `classify_and_act` |
| Verifiers could inherit the author's context | **Adversarial-verification default contract**: verifier nodes run `ReadOnly` + no inherited context (anti self-preferential-bias) |
| No trust boundary / model hint on nodes | **W3 quarantine** (`trust`) and **W4 model routing** (`model_hint`) carried to every spawn descriptor |

### Added

#### Core — orchestration primitives

- **Dynamic control-flow node kinds** (`orchestration::workflow::NodeKind`) — `Loop{max_iters}` (re-run until `loop_continue=false` or the cap), `Classify{branches}` (route to one branch by the node's `classify_branch` result, prune the rest), `Tournament{entrants}` (a *controller* node that generates entrants then pairwise-judges them to a winner via `tournament_winner`). All driven by the single workflow executor; additive ABI (`loop_continue` / `classify_branch` / `tournament_winner` result fields, `judge_match` spawn field).
- **`orchestration::tournament`** — single-elimination bracket (round-batched parallel judges; bye/odd handling), now the **kernel-internal** bracket core behind `NodeKind::Tournament` (no longer an SDK-exposed standalone primitive).
- **`orchestration::workflow`** — declarative `WorkflowSpec`/`WorkflowNode` (role/isolation/inheritance/model_hint/trust/deps) with `validate()` + `to_task_graph()`; templates `fanout_synthesize`, `generate_and_filter`, `verify_rules`, `classify_and_act`. `NodeTrust{Trusted,Quarantined}` (W3); `model_hint` (W4).

#### Core — W0 kernel-resident workflow executor

- **`scheduler::workflow_run::WorkflowRun`** — holds the DAG, spawns ready nodes as gated batches (each via `evaluate_syscall(Spawn)`), advances on completions, and `resume()`s from already-completed node ids.
- **`KernelInputEvent::LoadWorkflow { spec, parent_session_id, resumed_completed }`** + observations `WorkflowBatchSpawned` (carries each node's goal + role/isolation/trust/model_hint) and `WorkflowCompleted`.

#### SDKs

- **Node:** `RuntimeRunner.runWorkflow(spec, {resumedCompleted})`, workflow templates, `workflowSpecToKernel`. Control-flow node kinds (Loop/Classify/Tournament) ride the existing `runWorkflow` drive. Real-model e2e for the workflow DAG.
- **Python:** `RuntimeRunner.run_workflow`, templates, `workflow_spec_to_kernel`.
- **WASM:** `runWorkflow` + templates (tsc-verified).
- **Rust SDK:** re-exports `WorkflowSpec`/`WorkflowNode`/`WorkflowRun`/`JudgeMatch`/templates from `deepstrike-core`.

### Notes / deferred

- WASM workflow drive is type-checked but not runtime-tested (needs `wasm-pack`); Rust SDK has no sub-agent orchestrator, so it gets type re-exports, not a `run_workflow` drive.
- Runtime claim-extraction (dynamic verifier fan-out) is a follow-up; `verify_rules` static shape ships here.
- **Resume persistence to the session log** now ships across Node, Python, and WASM: `workflow_node_completed` events are persisted after each node, `recoverCompletedWorkflowNodes` extracts completed agent_ids from the log, and `resumeWorkflow`/`resume_workflow` drive recovery. The kernel's resume primitive (`WorkflowRun::resume`) correctly filters completed nodes from the batch via `load_workflow_resumed`.

---

Provider replay protocol fidelity: the provider layer now owns capture, validation, and protocol-scoped replay, and the shared recovery layer stopped fabricating provider-specific shapes — fixing reasoning/tool 400s and cross-provider replay pollution. The contract is consistent across Node, Python, WASM, and the Rust core.

### What this enables

| Before | After |
|---|---|
| Recovery layer synthesized Anthropic `native_blocks` for **any** tool turn lacking a stored replay (all SDKs + core) | `normalize_llm_completed` is **provider-neutral**: it passes the stored envelope through verbatim and never synthesizes a protocol shape |
| Rebuilt history seeded blindly into whatever provider ran next | **Protocol-aware seeding**: a stored envelope is only seeded into a provider speaking the same wire protocol; cross-protocol fallbacks re-serialize neutral context |
| DeepSeek reasoning + tool turns could ship requests the provider rejects (missing `reasoning_content`) | **Fail fast** with a provider/model/tool-id error before dispatch; non-empty `reasoning_content` captured into a provider-scoped envelope (stream **and** non-stream) |
| Replay envelope metadata could leak into OpenAI-compatible request messages | Only **wire-safe** fields (`reasoning_content` / `reasoning_details`) are merged; `schema_version`/`provider`/`protocol`/`native_message` stay out of the request |
| MiniMax was Anthropic-only | Split into **`MiniMaxAnthropicProvider`** (native_blocks) and **`MiniMaxOpenAIProvider`** (`reasoning_split`, preserves `reasoning_details`) |

### Added

- **`ProviderDescriptor`** (Node/WASM/Python) advertising `{provider, protocol, model, reasoning, toolCalls}`; descriptors on Anthropic/OpenAI-chat/DeepSeek/MiniMax providers drive protocol-aware recovery instead of class-name guessing.
- **OpenAI-compatible replay validator** (Node + Python): strict tool-result pairing (orphan/duplicate rejection) and a DeepSeek/MiniMax non-empty-`reasoning_content` requirement, run before request construction.
- **`MiniMaxOpenAIProvider`** + `minimax.openai` endpoint profile (Node); `reasoning_split: true` by default; `reasoning_details` survives replay.
- `isReplayCompatibleWithProvider` / `is_replay_compatible_with_provider` + protocol inference for legacy envelopes (by shape).

### Changed

- **Anthropic legacy reconstruction** moved out of the shared recovery layer into `AnthropicProvider.seedProviderReplay` (Node/WASM/Python): legacy logs without a persisted replay reconstruct neutral text + tool_use blocks at seed time, scoped to the Anthropic provider.
- **Rust core** `ProviderReplay` preserves unknown envelope fields (`#[serde(flatten)] extra`) so SDK-owned protocol metadata round-trips losslessly; `repair_llm_completed` no longer rewrites `provider_replay`.
- DeepSeek `complete()`/`stream()` capture a versioned, provider-scoped replay envelope only when real reasoning was produced (no `""` synthesis).

### Breaking

- **`MiniMaxProvider` removed** in favor of `MiniMaxAnthropicProvider` (Node + Python exports updated; no alias). Use `MiniMaxOpenAIProvider` for the OpenAI-compatible endpoint.
- Core `runtime::{synthesize_provider_replay, effective_provider_replay}` removed (provider synthesis is no longer a core responsibility).

## [0.2.8] - 2026-06-09

Provider-replay fallback routing: embedders running a multi-provider fallback chain can now pre-empt and recover from reasoning-replay validation failures instead of failing the whole request. Node + Python SDKs (the only SDKs that carry the OpenAI-compatible reasoning-replay validator).

### Added

- **Pre-flight query** `provider.assessReplayability(context, extensions?)` → `{ ok, offendingCallIds }` (Node) / `assess_replayability(context, extensions)` → `{ ok, offending_call_ids }` (Python): ask, before sending, which assistant tool-call turns lack the non-empty reasoning replay a reasoning-requiring provider (DeepSeek / MiniMax) needs — so a fallback host can keep thinking on, disable it, or skip the candidate. Runtime helper `assessProviderReplayability` / `assess_provider_replayability` treats providers without the hook as `ok`.
- **Graceful degradation opt-out** `extensions.degradeMissingReasoningReplay` (Node) / `extensions["degrade_missing_reasoning_replay"]` (Python): when a reasoning-requiring tool-call turn has no stored reasoning, serialize it with a minimal placeholder (`DEGRADED_REASONING_PLACEHOLDER`) so a recovery/fallback request goes out degraded-but-successful instead of throwing. Opt-in, never the silent default. The control flag is stripped centrally (`INTERNAL_EXTENSION_KEYS`) and never leaks onto the wire request.

### Fixed

- Strict tool-result pairing now rejects the **missing case** — an assistant `tool_calls` turn whose `tool_call_id`s are never answered before the next assistant/user turn — at the SDK layer, alongside the existing orphan/duplicate checks, instead of surfacing later as a gateway `400`.

## [0.2.6] - 2026-06-03

Agent OS consolidation release: M1 scheduler authority, M2 resource quotas with enforcement, M3 handle residency and Layer-4 read-time projection, native profile helpers across host SDKs, and configurable memory policy at the WriteMemory/QueryMemory traps.

### What this release enables

| Before (0.2.5) | After (0.2.6) |
|---|---|
| Scheduler and process views partially duplicated in SDK | `schedule()` is authoritative; task/process state unified under M1 consolidation |
| Governance gate without per-resource budgets | M2 **resource quotas** via `set_resource_quota` — syscall trap enforces limits before tool I/O |
| Layer-4 collapse removed messages in-place | **Read-time projection** via live `HandleTable` index; spool residency activated (M3.3) |
| Memory validation rules fixed at compile time | **`set_memory_policy`** — toggle validation, cap `retrieval_top_k`, override size limits at runtime |
| OS profile helpers only in Node | `assertNativeProfile` / `osProfile` + quota wiring in **Node, Python, Rust, WASM** |

### Added

#### Core — M1 consolidation

- **`schedule()` authoritative:** Scheduler owns next-action decisions; legacy ProcessTable scaffold removed in favor of TaskTable view.
- **Phase 0 regression baseline:** Budget-axis and AgentProcess-view tests pin consolidation contracts.

#### Core — M2 resource quotas

- **`set_resource_quota` ABI:** Per-resource limits enforced at the syscall trap before tool execution.
- Kernel tests and state-machine wiring for quota exceed observations.

#### Core — M3 handle residency (3.3a–3.3c)

- **M3.3a — `HandleTable`:** Live index over working-context tool results.
- **M3.3b — Layer-4 read-time projection:** Context collapse replaced by handle residency + projection at render time.
- **M3.3c — Spool residency:** Layer-1 spool refs integrated into handle table; dead `CollapseMode` scaffold removed.

#### Core — Memory policy enforcement

- **`MemoryPolicy` installed via `set_memory_policy`:** `validation_enabled`, `retrieval_top_k`, `max_content_bytes`, stale-warning config.
- WriteMemory / QueryMemory traps honor policy (`validation_enabled: false` bypasses rules; `retrieval_top_k` clamps query requests).

#### SDK — Native profile + resource quota parity

- **`assertNativeProfile` / `osProfile`** exported from Node, Python, Rust, and WASM runners.
- **`set_resource_quota`** loaded through host runners before `start_run`.
- **`memoryPolicy` / `memory_policy`** wired in Node, Python, Rust, and WASM (→ `set_memory_policy`).
- **Config-shape isomorphism:** all four SDKs now expose the same 8 config-in options (`governancePolicy`, `attentionPolicy`, `schedulerBudget`, `resourceQuota`, `memoryPolicy`, `osProfile`, `tokenizer`, `enablePlanTool`). WASM previously lacked `tokenizer` / `enablePlanTool` — both added (`set_tokenizer` / `set_plan_tool_enabled` wiring).
- **`scripts/check-sdk-parity.mjs`:** Expanded markers for os-profile, resource-quota, and memory-policy surfaces (per-SDK memory-policy checks).

#### SDK — Stability example

- **`node/examples/long-running-stability.mjs`:** Multi-turn validation harness (tools, skills, memory, spool, wake, quotas).

#### Tests

- `node/tests/runtime/memory-policy.test.ts` — kernel ABI reference tests for policy config and enforcement.
- `python/tests/test_resource_quota.py`, Rust/WASM native-profile and resource-quota tests.

### Changed

- **Phase 4 cleanup:** Removed standalone `ProcessTable` and dead compression scaffold after M1/M3 consolidation.
- **Documentation:** Kernel ABI and SDK parity matrix updated for M1/M2/M3 and memory policy; package READMEs note quota and policy APIs.

### Fixed

- **`initialMemory` on Python / WASM:** both runners emitted the removed `add_memory_message` event, which the kernel rejects (unknown `kind`) — any run setting `initial_memory` / `initialMemory` failed during setup. Migrated to `add_knowledge_message` (same `content` / `tokens` fields), matching the Node runner.

### Notes

- Rebuild Node native bindings after upgrade: `cd crates/deepstrike-node && napi build --platform --release`.
- Python: `maturin develop --release` for the latest kernel ABI including `set_memory_policy` and `set_resource_quota`.
- WASM: rebuild the bundle (`npm run build:wasm`, requires `wasm-pack`) so the `.wasm` embeds the updated core — without it the new config-in events are accepted but not enforced.

## [0.2.5] - 2026-06-02

Agent OS release: kernel three-primitives refactor (M0–M4), OS native profile defaults, Layer-1 large-result spool, semantic page-out pipeline, and Phase-7 memory syscalls — across core, Node, Python, Rust, and Wasm event mapping.

### What this release enables

These mechanisms move the SDK from “agent loop library” to an **Agent OS runtime** — kernel-mediated decisions, SDK-owned I/O. Practical capability gains:

| Before (≤ 0.2.4) | After (0.2.5) |
|---|---|
| Scheduling, compression, and permission logic scattered in each SDK | Unified syscall trap, TCB lifecycle, and MM eviction funnel — same semantics in Node, Python, and Rust |
| Large tool outputs and long sessions hit token walls | Layer-1 spool (preview + `.spool/` ref) and semantic page-out → `DreamStore` keep runs going without hard truncation |
| Governance and signal routing were optional SDK plugins | OS native profile: declarative `governancePolicy` and in-kernel `attentionPolicy` on by default |
| Long-term memory mostly via meta-tools and idle pipelines | `writeMemory` / `queryMemory` kernel syscalls with validation, audit events, and retrieval closure |
| Session logs skewed toward chat + tools | Full OS event stream (`syscall` · `sched` · `mm` · `proc` · `ipc`) and rebuildable OS snapshots |

**For application developers:**

1. **Less runner glue** — feed events, execute I/O, drain observations; avoid reimplementing sched/compress/govern/signal logic per product.
2. **Heavier workloads** — multi-hour runs, large diffs, batched tools, and sub-agents have explicit kernel + SDK paths (spool, page-in/out, process table, suspend/resume).
3. **Enterprise-ready defaults** — policy gates, signal disposition, memory validation, and audit counters are first-class, not fork-the-kernel add-ons.
4. **Cross-language parity** — one session-log contract and replay semantics across Node, Python, and Rust.

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
- **Documentation:** Node and Python READMEs expanded; VitePress docs add [Agent OS](docs/concepts/agent-os.md), updated architecture, kernel ABI, SDK parity matrix, and SDK guides for 0.2.5.
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
