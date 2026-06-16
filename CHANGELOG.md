# Changelog

All notable changes to DeepStrike are documented here.

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- **`InMemoryDreamStore` — a process-local `DreamStore`.** Previously lived as `MockDreamStore` in
  the Node SDK test helpers; promoted to a public export so benchmarks, examples, and downstream
  consumers can use it without copying the boilerplate. Initial-memory seeding (handy for memory
  A/B scenarios), `addSession` / `addMemories` for test setup, `savedSessions` for assertions.
  `search()` is a non-semantic top-K slice — the kernel ranks by score, so insertion order is what
  surfaces; plug in a real store for semantic search. **All four SDKs** — `InMemoryDreamStore`
  (Node `@deepstrike/sdk`, WASM `@deepstrike/wasm-kernel`, Rust `deepstrike-sdk`) and Python
  `deepstrike.InMemoryDreamStore`.

## [0.2.20] - 2026-06-16

### Added

- **`ReplayProvider` — a request-skipping `LLMProvider` for deterministic re-runs.** Wraps a recorded
  `Message[]` queue and emits `usage` / `text_delta` / `tool_call` stream events from each, never
  hitting an API. Designed for benchmark cross-variant cost comparisons, deterministic CI test
  fixtures, and golden regression checks. Distinct from `seedProviderReplay` / `peekProviderReplay`
  (which is the unchanged session-repair reasoning-content cache and does NOT skip LLM calls).
  Optional tokenizer hook (defaults to `chars/4`); optional `wrap` mode for loops past the fixture
  length; `reset()` rewinds for reuse across sessions. **All four SDKs** — exported as
  `ReplayProvider` (Node `@deepstrike/sdk`, WASM `@deepstrike/wasm-kernel`, Rust `deepstrike-sdk`)
  and `ReplayProvider` Python class (in `deepstrike`).
- **`extractRecordedMessages(events)` — fixture helper.** Walks a session-log's `llm_completed`
  events and produces the `Message[]` queue `ReplayProvider` consumes. Accepts both wire shapes the
  SDK uses interchangeably (camelCase in-memory and snake_case on-disk), so a prior `SessionLog`
  is a drop-in replay fixture. **All four SDKs** — exported as `extractRecordedMessages`
  (Node/WASM/Rust) and `extract_recorded_messages` (Python). Rust additionally exposes
  `extract_recorded_messages_from_entries` for the `SessionEntry { seq, event }` wrapper shape
  `SessionLog::read()` returns.

- **`judge()` — one-shot quality scoring against goal + criteria.** Public wrapper around the
  kernel's `gen_eval` free functions (`buildEvalMessages` / `parseVerdict` / `verdictOutputSchema`,
  folded out of the old `EvalPipeline` class in 0.5.0). Renders the eval prompt, streams a verdict
  from the supplied provider, parses it into a typed `Verdict { passed, overallScore, feedback,
  details[] }`. Single LLM call, no retry loop or skill extraction — for the full retry/refine flow
  use `HarnessLoop`. **All four SDKs** — `judge` is async on every port; the building-block
  functions (`buildEvalMessages` / `parseVerdict` / `verdictOutputSchema`, snake_case on Python)
  are sync on Node/Python/Rust and async on WASM (lazy kernel load). Public types: `Criterion` /
  `Verdict` / `VerdictDetail` / `JudgeArgs` (Node/WASM); same shapes as dataclasses on Python and
  re-exports of `deepstrike_core::harness::eval` types on Rust.

## [0.2.19] - 2026-06-16

### Added

- **Skill/run tool gating — narrow the tool schemas exposed to the model.** Two opt-in, additive, zero-default-change mechanisms that cut per-turn tool-schema exposure (measured −65% on a ~30-tool surface):
  - **Static per-run profile.** `RuntimeOptions.allowedToolIds` (Node/WASM/Python `allowedToolIds` / `allowed_tool_ids`; Rust `allowed_tool_ids`) exposes only the listed tool ids (plus the skill/memory/knowledge/update_plan meta-tools) for the whole run. It lowers to the same `AgentRunSpec.capability_filter` sub-agents already use — byte-stable across the run, so it never busts the prompt-cache prefix. **Zero new ABI** (reuses the existing run_spec wire).
  - **Epoch skill gating.** A skill that declares `allowed_tools` in its frontmatter (Node/WASM `.md`; Python `.md`; Rust `.md` / `.json` / `.py`) narrows the exposed toolset to `meta-tools ∪ stable-core ∪ ⋃(active skills' allowed_tools)` from the turn it loads onward. The kernel tracks an active-skill set (new `SkillActivated { name }` event; the SDK emits it when a `skill` call resolves and re-emits from replayed history on wake) and resolves each skill's tools from the catalog. `RuntimeOptions.stableCoreToolIds` (new `SetStableCoreTools` kernel event) configures the always-exposed safety set. The toolset is **byte-stable within an epoch** and only changes when the active set changes, so the prompt cache busts at most once per skill load. A skill that declares no `allowed_tools` does not narrow (errs-open / back-compat).
  - **Per-turn telemetry.** `RuntimeOptions.onTurnMetrics` (Node/WASM/Rust) / `on_turn_metrics` (Python) emits a `TurnMetrics` per LLM turn — `toolsExposed` / `toolsCalled`, the prompt-cache split (`cacheReadTokens` / `cacheCreationTokens`), `inputTokens`, and the `activeSkill` (for dwell). Pure observation; a throwing sink never breaks the run.

### Fixed

- **Skill `allowed_tools` was dropped end-to-end.** The frontmatter field was declared on the kernel `SkillMetadata` but never parsed (Node/Python loaders), never forwarded (`skillMetadataToKernel`), and never read. The Node and Python runners also re-mapped `SkillMetadata` field-by-field at registration, silently dropping it. The full pipe is now wired across all four SDKs.

## [0.2.18] - 2026-06-15

### Changed

- **BREAKING — `EvalPipeline` folded into the workflow substrate (OS-axis #6).** The standalone `EvalPipeline` state-machine class and its FFI bindings are **removed** (Node `EvalPipeline`/`EvalPipelineAction`, Python `EvalPipeline`/`EvalPipelineAction`, WASM `EvalPipeline`, and the kernel `harness::eval_pipeline` module). The generate→evaluate→retry quality gate's compute is now **stateless free functions** exposed on each port: `buildEvalMessages` / `parseVerdict` / `verdictOutputSchema` (Node/WASM camelCase; Python `build_eval_messages` / `parse_verdict` / `verdict_output_schema`). The quality gate's declarative form is the new **`gen_eval` workflow template** — a `Loop` worker + a bias-resistant `Verify` eval node carrying `verdictOutputSchema` as its `output_schema` (kernel `orchestration::workflow::gen_eval`, mirrored as `genEval` / `gen_eval` in the Node/Python SDKs). `HarnessLoop`'s public surface (`HarnessOutcome`) is unchanged — it now drives the loop with the free functions instead of the state machine. **Migration:** replace `new EvalPipeline({extractSkillOnPass}); p.feedOutcome(...); p.feedEvalResult(...)` with `buildEvalMessages(goal, criteria, result, attempt, extractSkillOnPass)` + `parseVerdict(text)`; or use `HarnessLoop` (unchanged) / the `gen_eval` template.

### Added

- **OpenAI Responses continuation — Python parity (prefix-cache G1).** New Python `OpenAIResponsesProvider` (+ `OpenAIResponsesAdapter`) mirroring Node: `previous_response_id` keeps history server-side so each turn sends only the uncovered tail, with automatic degrade-to-full on a missing/expired chain. Continuation state lives in the provider-owned `ProviderRunState`. (Node has shipped this since 0.2.12; this closes the Python gap. WASM/Rust stay minimal.)

### Fixed

- **Rust SDK didn't handle the `AgentPreempted` observation** added in 0.2.17 — its `KernelObservation` match was non-exhaustive, so `deepstrike-sdk` failed to compile (`cargo test --workspace` was red). Added the missing no-op arm.
- **Stale signal-preemption test.** `t02_state_machine::high_urgency_signal_injects_note` asserted a High-urgency signal injects the hard `[INTERRUPT]` marker, but 0.2.17 made High a *soft* `Interrupt` that records a `[SIGNAL]` note (the `[INTERRUPT]` marker is now exclusive to Critical/`InterruptNow`). Test updated to match the shipped semantics.

## [0.2.17] - 2026-06-15

### Added

- **Agent-authored workflows (M5/G1).** An agent can now author and run its own workflow DAG. `start_workflow` lets a workflow node flatten a whole sub-spec onto the running DAG, and a top-level agent calling `start_workflow` mid-conversation auto-pivots its run into driving the authored workflow (kernel `Syscall::LoadWorkflow` + `KernelInputEvent::SubmitWorkflow` + `RuntimeRunner.bootstrapWorkflow`, **bootstrap-or-flatten** under one kernel / one quota). Node, Python, WASM.
- **Mid-flight signal preemption.** A Critical `InterruptNow` signal aborts in-flight work instead of only being noted in context. The kernel distinguishes soft `Interrupt` (handled at the next turn boundary) from hard `InterruptNow` (preempt running sub-agents/workflow → mark `Done(UserAbort)`, tear the owning workflow down, emit the new `AgentPreempted` observation, reclaim the root). The SDK cancels the in-flight LLM call: `AbortSignal` → provider socket abort on Node/WASM (anthropic/openai/fetch), asyncio `task.cancel()` on Python. A concurrent monitor delivers signals during a running workflow batch.

### Changed

- **Kernel microkernel split.** `scheduler/state_machine.rs` decomposed into `state_machine/{mod,gate,signal,capability,eviction,tests}.rs` — the syscall gate, signal routing, and capability ops are now focused modules and the state machine is the dispatcher. No behavior change.
- Removed the dead `Disposition::Transform` variant.

## [0.2.16] - 2026-06-15

### Fixed

- **Meta-tool leakage in workflow child runners.** Child nodes wasted turns calling meta-tools (`skill`, `memory`, `knowledge`, `update_plan`) that were always whitelisted regardless of the node's `permitted_capability_ids`. Two root causes fixed:
  - **Source leakage:** child runners inherited parent's `skillDir`/`dreamStore`/`knowledgeSource`/`enablePlanTool` via spread operator, causing the kernel to register meta-tool schemas the LLM would then call. Now only sources whose meta-tool is in `permitted_capability_ids` are passed to the child runner.
  - **Filter leakage:** `FilteredExecutionPlane` hardcoded `DEFAULT_META_TOOLS` as always-allowed. The orchestrator now derives the `metaTools` set from the manifest and passes it explicitly, so only permitted meta-tools are admitted.
- Applied consistently across all three runtimes: Node (`sub-agent-orchestrator.ts`), Python (`sub_agent_orchestrator.py`), and WASM (`sub-agent-orchestrator.ts`).
- **`renderedContextToSdk` dropped `state_turn` and `frozen_prefix_len` from the kernel JSON step path.** The kernel emits these fields in the `call_provider` action's `RenderedContext`, but the Node and WASM SDK's JSON→SDK mapper (`renderedContextToSdk`) never read them — so `stateTurn` was always `undefined` and `frozenPrefixLen` was always missing in the provider context. The NAPI `render()` path (used only for reactive-compact retries) was correct; the primary `step()` path was not. Python was already correct.

## [0.2.14] - 2026-06-14

### Fixed

- **stateTurn serialization unified across all providers.** The volatile State turn was rendered through content-only serializers (`toAnthropicContent` / raw `.content`) instead of the full message path, silently dropping `toolCalls` (→ missing `tool_use` blocks) and `contentParts`. Affected: Anthropic (Node, Python, WASM), OpenAI Responses (Node). All four now go through the same `toAnthropicMessages` / three-branch expansion that history turns use.
- **WASM `toAnthropicMessages` tool_result serialization.** Tool messages were serialized with a hardcoded empty `tool_use_id` and `is_error: false` instead of reading `contentParts` for the correct `callId`, `output`, and `isError`. Now matches the Node/Python implementation.
- **WASM `RenderedContext` type parity.** Added missing `frozenPrefixLen` field (P1-E deep cache anchor was silently lost). Removed phantom `systemVolatile` field (kernel never emits it; was dead code).
- **WASM Anthropic P1-E deep anchor.** `applyMessageCacheControl` now accepts `frozenPrefixLen` and pins a stable breakpoint at the compaction boundary instead of always rolling, matching the Node/Python implementation.

## [0.2.13] - 2026-06-14

### Fixed

- **TaskLane is now a freeform string instead of a strict enum.** The kernel previously defined `TaskLane` as a four-variant Rust enum (`orchestrate`, `implement`, `retrieve`, `verify`), causing `serde` deserialization to reject any custom lane value (e.g. `"prd-fill"`, `"eval"`). Since the kernel never reads `lane` for scheduling — it is purely a classification label that round-trips through `KernelInput` — the type is now a `#[serde(transparent)]` `String` newtype with well-known constants. Downstream apps can use any lane string without kernel changes.

## [0.2.12] - 2026-06-14

Render→provider optimization release. Three converged workstreams, all in the layer that turns kernel context into a provider wire request — and all orthogonal to the Agent OS (partition/scheduler) and dynamic-workflow (orchestration DAG) layers: **(1)** provider prompt-caching so multi-turn history actually caches, **(2)** a deeper prefix-cache / attention pass (metrics, monotonic collapse, two-tier breakpoints), and **(3)** correct multimodal image input. The axiom behind all three: prompt-cache, KV-cache, and attention reward a single shape — a long, **byte-stable, position-0-contiguous prefix** with the volatile-but-important content pushed to the **end**.

> **Activation note.** The kernel-emitted fields (`state_turn`, `frozen_prefix_len`) are additive + dual-path: Rust (source-linked) is live; Node/Python/WASM activate them on the next native binding rebuild (`napi` / `maturin` / `wasm-pack`, performed at publish) and run **byte-for-byte at prior behavior** until then (provider reads the field when present, falls back to `turns[0]` when absent). Multimodal image **upload + use works without a rebuild** — it rides the already-shipped `add_history_message` kernel event plus pure-SDK serialization.

### Added

**Prompt caching — multi-turn history (all four ports)**

- **State turn separated from the cacheable history (the fix that makes multi-turn caching actually work).** The volatile State turn (task_state + signals, rebuilt every call) was rendered as `turns[0]` — at the *front* of the message array. Because caching is a prefix match, that volatile first message invalidated the entire message cache every turn, so the rolling breakpoints below produced **zero** history cache reads in any real multi-turn run (Anthropic's explicit cache *and* OpenAI/Gemini/Ollama's automatic prefix caches alike). The kernel now emits it as a separate `RenderedContext.state_turn` field with `turns` history-only; every provider renders it **last** (Anthropic after the cache breakpoint; OpenAI-family / Gemini / Ollama as the latest turn; Responses as a fresh input each turn). History becomes a byte-stable cacheable prefix — live task state lands by recency. ABI added across the kernel + all three native bindings + four SDK type defs.
- **Rolling message-history cache breakpoints (Anthropic).** Two `cache_control` breakpoints roll across the conversation tail — the final message + the nearest preceding user turn (Anthropic's 20-block lookback bridges multi-block turns). With `systemStable` / `systemKnowledge` this writes the history prefix once and re-reads it every turn, collapsing input cost from ~quadratic to ~linear.
- **Cache-token usage surfacing.** Usage event / `TokenUsage` gain `cacheReadInputTokens` (~0.1×) and `cacheCreationInputTokens` (~1.25×). OpenAI-family map their figures in — `prompt_tokens_details.cached_tokens` (OpenAI/Qwen/MiniMax/GLM/Kimi), DeepSeek `prompt_cache_hit_tokens`, Gemini `cachedContentTokenCount`, Responses `input_tokens_details.cached_tokens`.
- **Deterministic `prompt_cache_key`** (Node + Python OpenAI-family) for steadier automatic-cache routing — FNV over system + tool names, overridable by the caller, ignored by non-OpenAI endpoints.
- **Cache-budget regression guard** + **Python system partition** (`RenderedContext` gains `system_stable` / `system_knowledge`, already exposed by the PyO3 binding).

**Prefix-cache & attention pass (P0–P1)**

- **Cache-reuse metrics (P0-A).** `PrefixFingerprint` (per-render hash of system blocks + each history turn) certifies the reuse contract — one render extends another iff system hashes match *and* the prior turn-hash vector is a prefix of this one; `cacheHitRate` (`cacheRead / inputTokens`) is the headline metric for a session. Pure/derived, never persisted.
- **Monotonic collapse (P0-C).** Layer-4 tool-result collapse is now one-way (`Resident → Collapsed`) within a cache generation, reset only at compaction/renewal boundaries. The old two-way version un-collapsed when pressure fell, rewriting mid-prefix bytes and invalidating the prompt-cache on every threshold oscillation; collapse now only rewrites the prefix at the moments it's already being rewritten.
- **Two-tier breakpoints (P1-E).** With `frozen_prefix_len` set, the Anthropic provider **pins a deep breakpoint at the frozen boundary** and rolls one at the tail (instead of the rolling pair), maximizing the stable cached span on long histories; dual-path falls back to the rolling pair when the field is absent.

**Multimodal image input (the framework couldn't upload/use images before)**

- **Upload.** `run({ goal, attachments })` seeds images/audio as a user history message via the `add_history_message` kernel event — injected before `start_run` so they land in the **first** render, and persisted in `run_started` for wake/resume. Node + Python.
- **Use.** Every provider now serializes image parts: fixed **Gemini** (Node + Python — `buildContents` sent only text, dropping images) and **WASM** (added `contentParts` to the `Message` type + image rendering in both converters); Anthropic / OpenAI-chat / Ollama / Responses / Rust were already correct. The core content model (`ContentPart::Image`) and all three bindings carry image parts end-to-end.

### Changed

- **Tool cache breakpoint dropped when system blocks carry one** — tools render before `system`, so the `systemStable` breakpoint already caches the tools prefix; the tool breakpoint is anchored only when `system` is an unpartitioned string, freeing a 4th slot for the message history.
- **`inputTokens` is the full prompt size** (uncached + cache read + cache write), not the uncached remainder — it feeds the kernel's context-pressure gate as the authoritative prompt size (uncached-only would suppress compaction until a 413). Cost = `inputTokens − cacheRead − cacheCreation`.
- **Anthropic streaming usage is max-accumulated** — input/cache counts pinned at `message_start`, a later `message_delta` may omit them; `Math.max` keeps totals from zeroing and lets the final output through.
- **Default protocol for DeepSeek, Qwen, GLM, Kimi switched to `anthropic-messages`.**  All four Chinese providers now offer Anthropic Messages API compatible endpoints; the framework defaults match the same dual-provider pattern MiniMax already used. Each vendor gains an `*AnthropicProvider` class (Node + Python) and a new endpoint profile (`deepseek.anthropic` → `api.deepseek.com/anthropic`, `qwen.anthropic` → `dashscope-intl.aliyuncs.com/apps/anthropic`, `glm.anthropic` → `api.z.ai/api/anthropic`, `kimi.anthropic` → `api.moonshot.ai/anthropic`). Model profile defaults (`defaultEndpointId`) point to the Anthropic endpoint for all chat models; embedding models stay on their existing endpoints. The OpenAI-based classes remain available and are selected when callers pass the `*.openai` endpoint explicitly — fully backward compatible.

### Notes

- **Verified live** against DeepSeek (`api.deepseek.com`): a second turn over a shared prefix reported `cacheRead = 2560 / 2645` input tokens — the entire `[system + history]` prefix served from cache, confirming the append-state-last design delivers real multi-turn cache hits on an OpenAI-family provider.
- Additive + dual-path throughout; **no config = prior behavior**; all wire changes pass golden + 4-SDK parity; pure/zero-I/O paths don't regress.
- Tests: core **447**; rust SDK **45** + integration **224**; node **280**; python **112**; wasm **43** + `tsc` clean. `verify-release.sh` green.

## [0.2.11] - 2026-06-12

Dynamic-workflow release: an orchestration consolidation (R1/R2), the **runtime DAG-append syscall** (`SubmitNodes`), and the four structural gaps (G1–G4) surfaced by comparing our orchestration-as-data engine against Claude Code's code-orchestration model (`agent()` / `parallel()` / `pipeline()` + the six patterns + quarantine + budget). G1–G4 are additive ABI, landed kernel-first then mirrored across node/python/wasm; golden fixtures byte-identical.

### Added

- **Runtime node-append (`Syscall::SubmitNodes`).** A running workflow node can append nodes to the live DAG via `submit_workflow_nodes` — true loop-until-done and per-item fan-out (the claim-extractor → one-verifier-per-claim shape). `depends_on` is batch-relative and backward-only; each appended spawn passes the same syscall gate (quota / depth / quarantine) as any node; submissions are recorded and replayed on resume. Governance backstop: a `max_workflow_nodes` quota caps runaway growth.
- **G1 — quarantine no-privilege-escalation (security).** `SubmitWorkflowNodes` now carries `submitter_agent_id`; `WorkflowRun::submit_nodes_from` coerces every node of a quarantined submitter to `Quarantined` (transitive taint), which the existing spawn-time quarantine gate then enforces. Closes the path where a quarantined node — having read untrusted content — escaped its sandbox by submitting a "trusted" / write-capable child.
- **G2 — deterministic compute nodes (`NodeKind::Reduce`).** A host-compute node that runs no LLM: the kernel schedules it like a `Spawn` but stamps its descriptor with a `reducer` name + dependency agent ids, and the SDK routes it to a pure registered function (`dedupe_lines` / `merge_json_arrays` / `concat` / `count`, plus user reducers) over those dependency outputs. Dedupe / filter / merge between stages without burning an agent — the "ordinary code between stages" of code-orchestration, expressed as a DAG node.
- **G3 — per-node `output_schema` (structured output).** `WorkflowNode.output_schema` rides to the spawn descriptor; the SDK instructs the agent, validates its output against a JSON-Schema subset, and re-runs the node once with the validation errors fed back on mismatch.
- **G4 — budget-as-signal.** `WorkflowBatchSpawned` carries a `WorkflowBudget` snapshot (node / concurrency headroom under the active quota); the runner injects a concise budget note into each node's goal so a coordinator can size its `submit_workflow_nodes` batch to real headroom instead of blindly hitting the cap. Omitted when no quota is installed.

### Changed

- **Error-terminated workflow nodes now fail instead of complete.** `record_completion` *fails* (rather than completes) a node whose agent returned `TerminationReason::Error`, so its dependents starve instead of running on missing / garbage input — a general correctness fix, and the lever G3 uses to fail a node whose output never conforms to its schema. Other terminations (max-turns / budget / timeout) still complete, since they may carry partial output.
- **`workflow_run` folded into the `workflow` module.** `scheduler/workflow_run.rs` had zero `scheduler/` dependencies (it imported only `orchestration/` + `types/`), so it was an orchestration concern historically misplaced under the scheduler. Moved into a directory module: `orchestration/workflow.rs` → `workflow/mod.rs` (the declarative spec), `scheduler/workflow_run.rs` → `workflow/run.rs` (the runtime). The public surface is unified under `orchestration::workflow::{WorkflowRun, WorkflowSpawnInfo, JudgeMatch, node_agent_id}`; the rust SDK re-export path updated accordingly. Pure move, history preserved as renames.
- **Documented the `EvictionOp` / `PressureAction` layer boundary.** A planned merge of the two was reframed after a closer read: `EvictionOp` is the planner-op vocabulary (per-op payload) and `PressureAction` is the pressure-level vocabulary (`recommend` / `should_compress` return value, `Ord` cascade key, wire label). They are distinct layers bridged once at `execute_eviction_op`, not a redundancy — documented as such so it isn't re-misframed.

### Removed (internal)

- **Dead orchestration scaffolding.** `gen_eval.rs` (`GenEvalLoop`), `planner.rs` (`build_graph`), and `executor.rs` had no callers — the live DAG path runs entirely through `WorkflowSpec::to_task_graph` and `WorkflowRun`. The one used helper (`executor::next_batch`) was inlined to `TaskGraph::ready_tasks`. No SDK bindings, no documented API. `orchestration/` goes from 6 modules to 3.

### Notes

- The originally-planned R2 consolidation (compaction dual-vocabulary collapse, signal → `schedule_multi`) was descoped after implementation-time findings: the dual vocabulary is two legitimate layers, and routing signals through `schedule_multi` is a no-op in the current single-task model (deferred to the future multi-task scheduler work).
- Tests: core **437**, node **266**, python **104**; wasm `tsc` clean; clippy 0 errors; golden fixtures byte-identical across all four SDKs.

## [0.2.10] - 2026-06-11

Compaction collapse: the 690-line compaction pipeline becomes a single planner decision point feeding pure mechanical executors, and the kernel now surfaces the **prompt-cache cost** of each compaction so the SDK can weigh tokens-saved against cache-rebuild. This release also fixes three regressions introduced by 0.2.9's W1 consolidation.

### Fixed (0.2.9 regressions)

- **Escalation suppression (B):** `should_compress` consulted post-paging *effective* ρ, so pressure escalation was silently suppressed after handles were paged out. Reverted to raw ρ for the escalation/trigger decision.
- **Label/log mismatch (C):** the `auto_compact` action was logged under another compactor's label because compactors self-summarised and self-logged. Compactors are now pure; the pipeline summarises and logs exactly once under the requested action.
- **Debug-assert abort (A):** an over-strict `debug_assert_eq!` on time-decay tripped (SIGABRT) when micro-compaction also emitted a time-decay op. Relaxed to an implication.

### Changed

- **Compactors → pure executors.** Selection logic (which oversized messages to snip, which tool-results to excerpt, how many oldest to drop) is lifted into pure planner helpers; `SnipCompactor` / `MicroCompactor` / `CollapseCompactor` / `AutoCompactor` no longer summarise, log, or select.
- **Cache-aware prefix protection.** Snip/excerpt skip the oldest `preserve_recent_turns` messages so they don't rewrite the Anthropic prompt-cache prefix — with a forced-compaction fallback (`prefix_keep` yields when there's no drop-room, so reactive 413 compaction still frees tokens).
- **Accurate cache cost on the `Compressed` observation.** Each step computes the real `prefix_invalidated_at`; the pipeline folds `min(...)` and surfaces it (plus tokens-saved) so the SDK can quantify *saved vs. rebuild*.

### CI

- Drop orphan `deepstrike-tokenizer` from `release-rust`: it isn't a workspace member and nothing depends on it, so `cargo publish -p deepstrike-tokenizer` failed on the first dry-run line and killed every Rust release. Publish `deepstrike-core` + `deepstrike-sdk` only.

### Tests

- Compaction golden tests recomputed for the prefix-protected behavior; new regression gates: `prefix_keep_yields_without_drop_fallback`, `pipeline_reports_accurate_prefix_invalidation`, `auto_compact_entry_logs_auto_compact_action`. Core 426 / fresh node 241 / fresh python 87 green.

## [0.2.9] - 2026-06-11

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

- **Standalone `Tournament` / `LoopUntilDone` primitives removed** from all SDKs (Node `createTournament`/`createLoopUntilDone`, Python `Tournament`/`LoopUntilDone`/`StopCondition`, WASM + Rust re-exports) in favor of the `NodeKind::Tournament` / `NodeKind::Loop` workflow nodes driven by the one executor (A#1 fold-in). Tournament's bracket logic survives as the kernel-internal `orchestration::tournament` core.
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
