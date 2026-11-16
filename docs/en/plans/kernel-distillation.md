# Kernel Distillation: One Implementation for Each of the Four Decisions

> Drafted 2026-11-15 from `main` (`df26eabf`, v0.2.83). Executor: Opus.
> Precedents: the [context-first plan](./context-lifecycle-first.md) (stages 0–9) and
> [DEC-13](../decisions/013-model-led-context.md).
> This plan covers `crates/deepstrike-core` only. The four SDKs and the ABI layer of
> `runtime/kernel/wire` are the next round.
>
> The Chinese version (`docs/plans/kernel-distillation.md`) is the working copy and is updated
> first; this translation follows it.

## 1. Goals

The framework's core principle is already written in the context-first plan:

> Under a finite attention budget, decide for every piece of information its **scope, form,
> lifetime and write authority**.

Stages 0–9 turned that sentence into `ContextItem` and a table of origin defaults
(`ItemOrigin::defaults` in `context/item.rs`). This round adds no mechanisms; it distills:

1. **The table says what the code does.** Every piece of model-visible information is a row of the
   item table, and each of the four decisions has exactly one implementation.
2. **The kernel does mechanics, not semantics.** The rule-based mechanisms left from before 0.2.82
   are measured one by one under DEC-13; those that do not help are deleted.
3. **Everything the kernel says to the model can be counted, turned off and replaced.**

Done means:

- Each of the four dimensions has one type and one point of enforcement (see §4).
- Every value in the origin table has a producer and a consumer; no value exists only in the table.
- `ContextConfig` / `ContextPolicy` have no field nobody reads and no switch that is off by default.
- Every mechanism in §3.5 has a recorded verdict here: "kept after measurement" or "deleted".
- The non-test line count of `context/` goes down (about 12.4k lines today, in-file tests
  included), and so does the field count of `ContextManager` (about 45 today).

## 2. Audit scope and confidence

Read in full: the structure of every module in `context/`; the implementations of `item.rs`,
`partitions.rs`, `compression.rs`, `renderer.rs`, `utility.rs`, `summarizer.rs`, `task_state.rs`,
`renewal.rs`, `config.rs`, `policy.rs`; the implementation or header of `mm/`, `syscall/`,
`signals/`, `governance/repeat_fuse.rs`, `scheduler/entropy.rs`,
`scheduler/state_machine/eviction.rs`; the type inventories of `wire/config.rs`, `wire/effect.rs`,
`wire/syscall.rs`.

Outline only: `scheduler/state_machine/mod.rs` (2.3k lines), `wire/driver.rs` (3.5k lines),
`wire/checkpoint.rs`, `orchestration/`. Conclusions that touch these files are marked "to verify";
read the code before acting on them.

"Unused" conclusions below come from grepping `crates/deepstrike-core/src`; the four SDKs and
`contracts/` were not searched. Before deleting any serde type or variant, search the SDKs,
`contracts/` and `tests/fixtures/`.

## 3. Today: how many implementations each decision has

### 3.1 The table covers 13 origins; only two kinds are really stored as items

| Information | Actual storage | Who manages its lifetime |
|---|---|---|
| knowledge (skill, recall, reference, blackboard) | `KnowledgePartition.entries: Vec<ContextItem>` | `ContextItem.lease`, table-driven |
| history (user, assistant, tool_result, provider_summary) | rows of a `Partition`, with an `ItemTag` | the compaction pipeline and renewal |
| signals, kernel notices | `ContextPartitions.signals: Vec<String>` | `LoopStateMachine.delivered_signals_len`: cleared by prefix length after the request commits |
| task state (goal, plan, directives, progress, scratchpad, blocked_on, recent_actions, compression_log) | 14 fields of `TaskState` | per-field caps (8 / 6 / 64, 3 rendered); renewal clears the scratchpad |
| note | `TaskState.notes`, with an item id but not a `ContextItem` | retired by the model; writes refused over budget |
| dependency output | `ContextManager.dependency_bodies: BTreeMap` | none |
| provider replay | `ContextManager.item_replays` | cleaned up with its item |
| skill activation | `ContextManager.active_skills: BTreeMap<_, Option<u32>>` | an expiry turn, plus `Lease::Until(SkillDeactivated)` for the body |

So a number of values in the table have no producer or consumer (grep in core, excluding
`item.rs` itself):

- `ItemOrigin::Signal`, `ItemOrigin::TaskState`, `ItemOrigin::Note`: no item of these origins is
  ever constructed.
- `Lease::SingleUse`: unused. The table says a signal is `SingleUse` ("leaves once referenced");
  the actual behavior is "leaves when the request carrying it commits". The two disagree.
- `Fidelity::Structured`: unused.
- `ItemScope::Agent`: no origin takes this value.

### 3.2 Form: three vocabularies

- `Fidelity` (`item.rs`): full / structured / excerpt / handle.
- `Residency` (`mm/handle.rs`): Resident / Collapsed / External / PagedOut.
  `ContextManager::history_fidelity` (`manager.rs:503`) translates it into `Fidelity` on read;
  `ItemTag.fidelity` itself records only excerpt.
- `ObjectDescriptor` / `ObjectKind` (`mm/handle.rs:274`): self-described as "an increment parallel
  to Handle", used for cross-task reads (about 37 references).

Two more changes of form belong to none of these vocabularies: narration collapse at render time
(`project_assistant_narration` in `renderer.rs`) and collapse previews. Both are temporary
projections inside render functions and leave no record on the item.

Of `ObjectKind`'s 9 variants, only `Artifact` is constructed anywhere in core (the first draft of
this audit wrongly named `HandleKind::WorkingPage` / `AgentResult`; all four `HandleKind` variants
are used through the wire string mapping). Its fate is folded into the `ObjectDescriptor` decision
of 1.4.

Compaction actions also have three vocabularies: `PressureAction`, `EvictionOp`,
`KernelPressureAction`. The parameters `EvictionOp` carries were dropped where it executed
(`Snip { per_msg_ratio: _ }`, `AutoCompact { preserve_turns: _ }` in `eviction.rs`), so planning
amounted to renaming. The bodies of `CollapseCompactor` and `AutoCompactor` were identical line for
line (`compression.rs:425–519`); they differed only in the target the caller passed.

### 3.3 Scope: enforced in two places, one of them a no-op

`ItemScope` has two consumers:

- `survives_renewal` (`renewal.rs:59`): only `Edge` returns false, and edge-scoped dependency
  outputs live in `dependency_bodies`, not in knowledge. This filter removes nothing today (to
  verify).
- `inherited_by` (`manager.rs:1681`): decides which knowledge a child run inherits; really in effect.

`MemoryScope` (tenant / namespace), `ContextInheritance` and workflow quarantine are isolation
boundaries on the effect-admission side. They are not merged in this round; the docs only need to
state how they relate to `ItemScope`.

### 3.4 Write authority: three dialects

- `Authority` (host / kernel / model / provider): checked on one path only, `push_knowledge_item`
  (`manager.rs:1683–1737`).
- `InputAuthority` + `SyscallCausation` (`wire/envelope.rs`, `wire/syscall.rs`): the ABI layer
  decides "who sent this input".
- `MemoryAuthor` + `MemoryTrustLevel` + attestation (`mm/memory.rs`, `mm/memory_candidate.rs`):
  write authority for long-term memory, the most complete of the three.

Write authority over task state is implicit: `TaskUpdate` has 7 fields, the `update_plan` schema
exposes 4 of them (plan, current_step, progress, blocked_on), and host commands write the rest. A
comment on `SyscallRequest::UpdateTask` says its authority "differs from `HostCommand::UpdateTask`",
but no table says who may write which field.

### 3.5 Where the kernel still judges for the model

DEC-13's Consequences section states that the rule-based mechanisms predating 0.2.82 were never
measured under the new standard. Listed one by one, with what each judges:

| # | Mechanism | Location | What the kernel decides for the model |
|---|---|---|---|
| M1 | narration collapse, on by default | `renderer.rs` `project_assistant_narration` | old assistant text has no value. 8.2 measured a loss of about 78k cached tokens per long-horizon task from it |
| M2 | utility-scored archiving | `utility.rs:137` | lexical overlap with the goal ×4000, an "unresolved" marker +20000, an "error/decision" marker +6000 |
| M3 | the six `RuleSummarizer` slots | `summarizer.rs` `classify_text` | sorts archived content into constraints / decisions / … by keyword and renders the latest 3 into the state block |
| M4 | five-level pressure ladder + idle decay | `config.rs`, `pressure.rs`, `mm/handle.rs` `plan_eviction` | thresholds 0.70 / 0.80 / 0.90 / 0.95 / 0.98 each trigger one rewrite; 60 idle minutes trigger micro |
| M5 | collapsing old tool results every turn once pressure passes the collapse threshold | `manager.rs` `recompute_handle_residency` | 8.0 found this to be one source of per-turn prefix breaks |
| M6 | knowledge budget eviction | `mm/value.rs`, `partitions.rs:447` | scores by kind weight, lexical reference count and an 8-turn recall lease |
| M7 | the state-block footer | `renderer.rs:185` `salience_footer` | `did:` / `next: advance the goal` / `must:` / `STOP:` |
| M8 | criteria gate | `state_machine/mod.rs:1274` | injects a self-check once when the model wants to finish |
| M9 | task-state fields that overlap notes | `task_state.rs`, the `update_plan` schema | `progress`, `blocked_on`, `scratchpad`: with notes on by default, the model has two places to record the same thing |

Two pieces of context explain why these are re-measured rather than deleted outright:

- **Notes changed the baseline.** 8.3's conclusion (deferring the light tier makes the model lose
  running tallies) was reached without notes. With notes on by default, every old conclusion about
  the compaction tiers must be re-measured on the new baseline.
- **M1's confounder is gone.** In 8.3, "keep narration until the boundary" correlated with loops
  because leaked DeepSeek markup stayed in context; 9.0 fixed that in the provider layer.

`RepeatFuse` (deny / terminate) is a breaker on the effect-admission side, not in this list, and
stays.

### 3.6 Configuration surface and stale text

- `ContextConfig.autocompact_buffer` and `autocompact_threshold()`: appear only inside `config.rs`;
  nobody reads them.
- `ContextConfig.verbose_control_notes`: a switch off by default, read in 3 files.
- `preserve_recent_units` (rendering) and `preserve_recent_turns` (compaction): two knobs with
  nearly the same meaning.
- Two types named `ContextPolicy`: `context/policy.rs` and `wire/config.rs`, with different fields
  (the latter adds `knowledge_budget_ppm`, `prompt_budget`, `delegated_tiers`).
- `ContextConfig::default()` set `notes_budget_tokens` to 0 while the wire default is 600, so
  tests that build a `ContextManager` directly got notes turned off.
- `EntropyWatchConfig.notify_model`: a switch off by default.
- Stale comments: `mm/handle.rs:233` and `manager.rs` mention a no-longer-existing `effective_rho`;
  `mm/handle.rs:415–418` says "the SDK upgrades summaries out of band with an LLM", which conflicts
  with DEC-13 rule 1 (verify whether any SDK still has that path); `MemoryTierHint::Semantic` is a
  leftover of it.
- Stale docs: Phase 4 of `docs/architecture/execution-model.md` says "Summarize is performed by the
  SDK-side summarizer".

### 3.7 What the kernel says to the model is spread over 11 files

Collapse placeholders, the footer, the criteria self-check, output-truncation notices, rollback
explanations, governance notices, budget warnings, note titles, `[context resumed]` and meta-tool
descriptions are spread over 11 files including `manager.rs`, `renderer.rs`,
`state_machine/mod.rs`, `task_state.rs`, `compression.rs`, `signal.rs` and `rollback.rs`. The plan
has said "instruction text is an experimental variable", yet today it cannot be counted or swapped
as a whole.

## 4. Target shape

**One table, four columns, one point of enforcement each.**

| Decision | The one type | The one point of enforcement |
|---|---|---|
| scope | `ItemScope` | renewal and spawn inheritance both call the same `visible_to(run)` |
| form | `Fidelity` (absorbing `Residency`) | the renderer only reads an item's fidelity; every downgrade is written back to the item |
| lifetime | `Lease` | one `sweep(turn, event)`: signals, skills, recalls and notes all go through it |
| write authority | `Authority` | one `admit_write(origin, writer)`: host commands, model tools and the kernel itself all pass it |

**All model-visible information is an item.** Signals, kernel notices, notes and dependency outputs
share one store; the four slots of `ContextPartitions` are only render positions derived from
origin and state (D3 of the context-first plan). Task state stays as one `Fidelity::Structured`
item, its fields trimmed by the outcome of stage 2 in §5.

**Compaction is "downgrade form by the table", not five algorithms.** The target shape (only
decidable after stage 2's measurements) has two actions: downgrade old tool results to handles
(readable again with `read_result`, so nothing is lost), and archive the oldest units at a
boundary. Which and when follow from an item's age and the budget, never from its content.

**The kernel's words have one source.** All text the kernel writes to the model lives in one module
and enters context as `origin = system` items; the lifecycle report states how many tokens of each
request are kernel text.

## 5. Tasks

Three kinds of task, with different rules:

- **R (refactor):** changes no byte of any render. Accepted when existing goldens are unchanged, or
  only checkpoint / report fields change.
- **D (delete):** code nobody uses. Accepted when it builds, tests pass, and no SDK or contract
  references it.
- **G (gated):** changes what the model sees. Decided by DEC-13 measurement; results are recorded
  here.

### Stage 0: cleanup (D, one PR)

- [x] 0.1 Delete `autocompact_buffer` and `autocompact_threshold()`.
- [x] 0.2 ~~Delete `HandleKind::WorkingPage` and `HandleKind::AgentResult`~~. On verification these
  variants do not exist; all four `HandleKind` variants are used. Cancelled; the fate of
  `ObjectKind` is folded into 1.4.
- [x] 0.3 Merge `CollapseCompactor` and `AutoCompactor` into one implementation that archives to a
  target; the two pipeline tiers keep their own `PressureAction` labels, and rendering and
  observations are unchanged.
- [x] 0.4 Either drop the `EvictionOp` parameters ignored at execution, or make execution use them —
  whichever leaves less code. Delete the paragraph on "future cache-cost ordering" from the
  `plan_eviction` docs (8.3 abandoned that direction).
- [x] 0.5 Fix stale comments and docs: `effective_rho`, "out-of-band LLM summaries in the SDK",
  Phase 4 of `execution-model.md`. Verify whether any SDK still has an out-of-band summary path;
  report it if so, do not handle it in this stage.
- [x] 0.6 `verbose_control_notes`: check whether any SDK exposes it; if not, delete the switch and
  its verbose branch.
- [x] 0.7 Make `ContextConfig::default().notes_budget_tokens` `DEFAULT_NOTES_BUDGET_TOKENS`; unit
  tests affected by it pass 0 explicitly.
- Acceptance: `cargo test --workspace` passes; kernel goldens unchanged byte for byte (if 0.7
  changes a golden, some test relied on the wrong default; fix each one and list them in the PR).
- **Record (2026-11-15, branch `refactor/kernel-distill-phase0`):**
  - 0.3 The two compactors became `ArchiveCompactor`; the two pipeline tiers are still labelled
    `ContextCollapse` / `AutoCompact`.
  - 0.4 Chose "drop the parameters": `EvictionOp::Snip` and `EvictionOp::AutoCompact` have no
    fields; `plan_eviction` lost its `preserve_turns` argument, and
    `ContextManager::plan_compaction_params` became `compaction_target_tokens`. Both are exported
    from the crate root, so this is a breaking change to the Rust public API, recorded in the
    CHANGELOG.
  - 0.5 Finding: **the Node SDK still had an out-of-band summary path**
    (`RunnerOptions.asyncSummarizer` → the `summary_upgraded` event, optional, off by default). The
    user decided to delete it: the `asyncSummarizer` option, the `AsyncSummarizer` type, the
    background rewrite and page-in, their tests and docs. The user then asked for full removal: the
    `summary_upgraded` event is gone from the contract, all four registries, and the Node / WASM /
    Python types and replay. Old logs still load (Node and WASM decoding does not reject unknown
    kinds); a `summary_upgraded` in them is ignored and replay uses the `compressed` event's
    rule-based summary.
  - 0.6 Also deleted `rollback_reason_message`, which served only the verbose branch.
  - 0.7 Four tests affected: `meta_tool_schemas_are_sorted` expects `note`;
    `top_level_run_without_spec_exposes_no_task_tools`,
    `exposure_baseline_narrows_even_without_a_ceiling` and `tests/rust`'s
    `agent_run_spec_capability_filter_enforcement` set `notes_budget_tokens = 0` (they test the
    task-tool surface).
  - Acceptance: `cargo test --workspace` passes; no golden fixture changed. Node (1421) and WASM
    (297) tests pass, SDK conformance included. The local Python venv's interpreter was broken, so
    pytest did not run; importing `session_log` under Python 3.12 confirmed the union matches the
    registry (51 event kinds).

### Stage 1: the table is the implementation (R, one PR per task)

Ordered by dependency. Each task removes one "value that exists only in the table".

- [x] 1.1 **Signals and kernel notices become items.** (2026-11-16)
  - `partitions.signals: Vec<String>` becomes item rows (origin `Signal` or `System`).
  - A new lease kind expresses "leaves when the request carrying it commits", replacing the
    `delivered_signals_len` prefix-count protocol. If `Lease::SingleUse` has no other use, rename it
    to this rather than keep both.
  - The lifecycle report's `history` tally shows signal counts and tokens.
  - Acceptance: the state block renders byte for byte the same; all signal-related state-machine
    tests pass; checkpoints with the old `signals` shape restore.
  - **Record.** Before starting, checked whether the prefix-count protocol had a real defect: it
    did. `delivered_signals_len` was not checkpointed, so a kernel restored from a checkpoint taken
    while a provider request was in flight forgot which signals that request carried and sent them
    again on the next request; the original kernel did not. The two diverged, which breaks replay
    determinism.
    - Each signal is a `SignalItem { text, delivered }` (`partitions.rs`): all are marked when a
      request is emitted, the marked ones leave when it commits, and the marks are cleared if it
      never goes out; `delivered_signals_len` is gone. Marked signals are always a prefix, so the
      checkpoint stores their count, `signals_delivered` (default 0, so old checkpoints restore as
      before).
    - `Lease::SingleUse` (used by no item, though the table gave it to signals) is renamed
      `Lease::UntilDelivered`, so the table says what the code does; the knowledge sweep does not
      expire items on it. The unit test of that unused path is deleted.
    - Regression test `a_delivered_signal_is_not_sent_again_after_a_restore`, failing without the
      change. No golden changed.
    - **Two deviations from the plan.** (1) Signals take no item id from the shared allocator:
      doing so shifts every later history item's id by one, renders carry `item_id`, and goldens
      would change, contrary to this task's "renders unchanged" acceptance; decide together with
      kernel notices in 1.7. (2) The lifecycle report gains no signal count or tokens: the `history`
      tally means "items that entered history" and signals never do; 1.7's `kernel_text_tokens`
      counts them instead.
    - `ContextPartitions.signals` is a public field read directly by `tests/rust`, so this is a
      breaking change to the Rust public API, recorded in the CHANGELOG and MIGRATION.
- [ ] 1.2 **Notes become items.** Each note in `TaskState.notes` is an item of origin `Note`;
  budget refusal, rewrite by title and the three graph relations are unchanged. `notes_tokens()`
  sums items.
  - Acceptance: the 8 tests in `context/notes.rs` pass without changing assertions; the `notes:`
    block is byte for byte the same.
- [ ] 1.3 **Dependency outputs become items.** `dependency_bodies` becomes items of origin
  `DependencyOutput`, scope `Edge`, fidelity `Handle`; `read_result("dep:<agent>")` reads the item.
  Only then does `survives_renewal` have something real to filter; add a test showing that
  dependency outputs after renewal behave as the table says. If today's behavior is "still readable
  after renewal", keep the behavior and fix the table.
- [ ] 1.4 **One vocabulary for form.** The four `Residency` states fold into the item's fidelity (a
  handle carries a locator and digest). `HandleTable` becomes an index from `call_id` to item and
  holds no state of its own. `history_fidelity()` is deleted.
  - Narration collapse and collapse previews become "downgrade written back to the item's
    fidelity; the renderer only reads". This changes where the collapse is recorded, not when it
    happens.
  - `ObjectDescriptor`: read how `scheduler/cross_operation.rs` and `wire/driver/syscall.rs` use it
    first, then decide whether it is a cross-task projection of an item (kept as a pure function)
    or a concept of its own (kept, with its boundary documented). Do not merge for the sake of
    merging.
  - Acceptance: golden renders unchanged byte for byte; the cache ledger's break attribution
    unchanged.
- [ ] 1.5 **One lifetime.** `sweep_expired_leases`, `sweep_expired_skill_leases` and
  `sweep_expired_item_leases` (called in a row at the top of `feed` in `state_machine/mod.rs`)
  become one table-driven sweep. `active_skills` expiry turns fold into the skill item's lease.
  `CapabilityLease` belongs to effect admission, stays separate, and is documented as such.
- [ ] 1.6 **One write authority.** Add `admit_write(origin, writer, op)`, with a table stating for
  each origin who may create, replace and delete. The three checks in `push_knowledge_item` move
  into it; each of `TaskUpdate`'s 7 fields names its writer, and a model write through
  `update_plan` to a field not on its list is refused.
  - Acceptance: one table-driven test enumerates every origin × writer × op and pins the result.
    Every place where today's behavior conflicts with the table is listed in the PR for the user
    to decide; behavior is not changed unilaterally.
- [ ] 1.7 **Kernel text in one place.** A new `context/voice.rs` (name negotiable) holds every
  string constant and format function the kernel writes to the model; the lifecycle report gains
  `kernel_text_tokens`.
  - Acceptance: renders unchanged byte for byte; grep finds no model-facing literal outside that
    module (tests excepted).
- [ ] 1.8 **Split `ContextManager` by responsibility.** After 1.1–1.7, when it has fewer fields.
  Split along the natural groups of the existing fields: item store, budget and pressure (estimate
  calibration included), cache ledger, capabilities and tool exposure (skills, capabilities,
  meta-tool schemas), reporting. Moves only, no logic changes.
- [ ] 1.9 Delete table values that still have no producer or consumer.

Stage 1 changes the checkpoint shape. Each task keeps a restore path for old checkpoints, with a
test, and records the change in `MIGRATION-0.2.83-to-<next>.md`. The host-facing ABI (`KernelInput`,
`KernelEffect`, the wire `ContextPolicy`) does not change in stages 0–1.

### Stage 2: the DEC-13 audit (G) — first round done, M1–M7 deleted

**Method: measure stripping everything first, then bisect.** Thirty pairs per mechanism would cost
too much.

1. On an experiment branch, give each of M1–M9 an experiment-only switch (never merged).
2. Define the variant `minimal`:
   - M1 narration collapse off;
   - M2 archive oldest first, force-keeping only the latest N units and unpaired tool calls;
   - M3 the summary becomes one mechanical line (units archived, tokens, tool names), no slots;
   - M4 no idle decay; snip and micro become one action, "downgrade old tool results to handles";
   - M5 collapse only in batches at boundaries;
   - M6 count only strong evidence as a reference, no kind weights;
   - M7 no footer;
   - M8 criteria gate off;
   - M9 `update_plan` loses `progress` and `blocked_on`; `scratchpad` is deleted.
3. Use DEC-13's default gate with **today's defaults as the candidate and `minimal` as the
   baseline**: the existing mechanisms, as a whole, must make success or cost per correct answer
   significantly better in their target scenarios, or they are deleted as a whole.
   - Scenarios: long-horizon (compaction-heavy, M1–M5), ledger (small window), multi-session (M9
     and its interaction with notes and memory), knowledge-pressure (M6); fan-out as regression.
   - 10 pairs each for a direction, then up to 30 for a verdict.
4. If today's configuration passes, bisect by group — compaction (M1–M5), prompting (M7, M8, M9),
   knowledge (M6) — until the mechanisms that really contribute are found.
5. Write every verdict back into this section: data, keep or delete, the deleting commit. Kept
   mechanisms lose their experiment switch; deleted ones go with their config fields, SDK options,
   contracts, tests and docs.

Notes:

- Rule out protocol-layer confounders before reading results (leaked tool markup, repeated call
  ids, runs that hit the turn limit); see 9.0 of the context-first plan.
- Measurement spends the user's money. Before starting, estimate the spend from existing data in
  `benchmark/context-lifecycle/results/`, check the DeepSeek balance, report the estimate to the
  user and wait for consent.
- If `minimal` sends many long-horizon runs to the turn limit, stop and analyse; do not run all 30.

**Record (2026-11-16).** The user approved a 30 CNY budget and decided to do stage 2 first and settle the rest of stage 1 after the audit.

- **Method.** On the experiment branch `exp/stage2-audit` (never merged), `DEEPSTRIKE_EXP_MINIMAL` strips M1–M9 to `minimal` (`all`, or a list such as `m1,m5`); unset, all 1158 kernel tests pass. Stage 2 did not wait for 1.4: the switches hook into the current code directly.
- **Measured** (`deepseek-flash`, paired by task, `results/stage2-*.jsonl`, `s2-default` as the candidate and `s2-minimal` as the baseline; about 15 CNY at long-horizon's historical rate):

  | Scenario | Pairs | Correct: default / minimal | Cost per correct answer | Gate |
  |---|---|---|---|---|
  | long-horizon | 10 | 6 / 4 | default 22% cheaper | too few pairs |
  | ledger | 30 | 28 / 28 | default 62% more | default not significantly better |
  | knowledge-pressure | 30 | 25 / 27 | default 76% more | **default significantly worse** |
  | multi-session | 48 sessions | 34 / 32 | default 9% cheaper | default not significantly better; by sequence 3 better, 1 worse |
  | fan-out | 12 | 12 / 12 | default 16% cheaper | regression only, no difference |

  - The cost difference is almost entirely the cache: minimal hits the cache more in every scenario; default breaks the prefix 2–7 times as often, almost all tagged `collapse` (M5) and `narration` (M1).
  - long-horizon answers were read one by one: wrong answers on both sides are counts off by 1–2 lines, with no error type unique to one side; the 6 discordant pairs split 4 to 2, within chance. minimal never hit the turn limit (0/10); the only turn-limit run was default's.
- **Decision (user, 2026-11-16): "delete most of them."** M1–M7 are deleted. M8 (the criteria gate, which fires only when the host passes acceptance criteria; no benchmark sets any, so there is no data) and M9 (`update_plan`'s progress / blocked_on and the scratchpad, part of the four SDKs' API) stay for now, to be decided separately. long-horizon was not topped up to 30 pairs.
- **How the deletion was done and checked.** First the switches were fixed (M1–M7 stripped, M8–M9 kept) and the regenerated goldens were byte-identical to the `minimal` measured on the experiment branch; then the dead code and the configuration surface were removed, after which the goldens differ only by the removed config fields and the digests that follow. In detail:
  - M1 narration collapse: projection, `collapseOldAssistantNarration` and the `narration` break cause removed.
  - M2 utility scoring: `context/utility.rs` replaced by `context/archive.rs`, oldest first, keeping only the recent units by rule.
  - M3 rule summary: `summarizer.rs` removed; a compaction's record is one line, `archived N messages (~T tokens); tools used: …`.
  - M4: idle decay (`idleMicroCompactMinutes`, two timestamps) and the separate snip tier removed; snip is micro's first stage, the thresholds lose `snip`, and `micro` defaults to 0.70 (how the measured minimal behaved).
  - M5: per-turn collapse of old tool results (`recompute_handle_residency`) removed; the `Collapsed` form is still produced by provider-side clearing and stays until 1.4.
  - M6: the knowledge retention score counts strong references only, without kind weights.
  - M7: the state-turn footer and the `recent_actions` log behind it removed.
  - Checkpoints read but no longer write `recent_actions`, `last_activity_ms` and `last_compact_ms`, so older checkpoints restore.
  - Tests: those asserting deleted behavior were removed; two Python tests that relied on the rule summary inflating pressure or on compaction timing now build their conditions directly (a long goal; looking for the re-activated skill in the state turn). All four SDKs' full suites, the reproduction script and the docs and contract checks pass.
- **Effect on stage 1.** 1.4 (one form) shrinks: narration collapse and per-turn collapse are gone. 1.7 has no footer or rule summary left to centralize. 1.2, 1.3 and 1.5 still show no concrete payoff; revisit them after 1.6 and 1.7.

After stage 2, converge the configuration: `PressureThresholds` keeps only thresholds still used;
`preserve_recent_units` and `preserve_recent_turns` merge; of the two `ContextPolicy` types one
stays authoritative and the other is renamed or deleted; `EntropyWatchConfig.notify_model` is
deleted under "no dormant switches" (needs the user's confirmation, §8).

### Stage 3: documentation

- [ ] 3.1 A new page `docs/concepts/context-item.md` holding only §4's table and the origin defaults
  table, stating each column's single point of enforcement in code. Other docs link to it instead
  of each explaining the partitions.
- [ ] 3.2 Update P3 in `docs/architecture/agent-os.md`, Phases 3–4 of `execution-model.md`, and
  `docs/guides/context-engineering.md`.
- [ ] 3.3 Sync `docs/en/`. The repository's docs check requires zh/en pairs (PR #4 once failed on
  it). This translation of the plan is part of that.

### Stage 4: the memory evidence chain, and measuring memory when it goes wrong

> Added 2026-11-15, after reviewing the context mechanisms since 0.2.78 against Anthropic's "The
> components of agent memory" (2026-11-06). The article itself was not read, only a summary of it;
> its experimental figures are not relied on.
> This stage goes beyond "`crates/deepstrike-core` only": 4.1 adds an optional field to a wire
> type, and 4.2–4.4 change the benchmarks and `deepstrike-lab`.

**Review.** The article splits memory into four layers (raw transcripts, notes, knowledge
representations, injected context) with an offline consolidation process moving knowledge between
them. Against the code:

| Article's layer | What exists | State |
|---|---|---|
| raw transcripts | the append-only session log; history items and handles, read back with `read_result` | exists. Since stage 0 removed `summary_upgraded`, no event rewrites replayed content |
| notes | the `note` tool, on by default; the kernel records `derived_from` / `links_to` / `supersedes` edges | exists, passed the gate |
| knowledge representation | the item graph, a projection within a run, not a store | exists |
| injected context | recall at the first turn and after each renewal, as index lines; point reads with `memory(record_id)` | exists. Refreshed only at boundaries, so a running session's knowledge base never changes behind its back |
| moving knowledge up | the model calls `propose_memory` | exists, passed the gate |
| offline consolidation | 9.4's consolidation run | deleted after measurement: multi-session 33/48 → 33/48, cost per correct answer +272% |

The article's "the framework sets boundaries, the model judges" and "forcing the model to record
only failures lowered the gain" are DEC-13's conclusion; they do not change stages 0–3. Two of the
gaps it points at are real:

- **The evidence chain breaks between notes and memory.** A note's evidence is resolved to items by
  the kernel (`note_evidence` in `notes.rs`) and recorded as edges. `propose_memory`'s
  `evidence_refs` are strings the model typed; `driver/syscall.rs` copies them into
  `CanonicalMemoryWrite` unresolved and unchecked. The `memory_written` event carries only
  `record_id`, `name`, `size_bytes` and `turn`. When a memory turns out wrong, the only way back is
  to find that `llm_completed` in the log by `session_id` and `turn`.
- **Memory that goes wrong was never measured.** Each multi-session sequence has two rules that
  never change, and the host accepts every proposal. So 9.4's conclusion is "consolidation does not
  help when memory is clean", not "consolidation does not help". Records are overwritten in place by
  `(scope, kind, name)` (`MemoryRecordStore::upsert`) and the model has no way to retire one;
  whether that is a problem had no data.

**Not doing**, and why:

- No `status` (candidate / verified / disputed / stale) on `MemoryRecord`, and no verification
  stage. Only the model or the host can verify; the "verified" axis already exists as
  `MemoryTrustLevel` plus attestation.
- No return of the consolidation run, and no "pluggable consolidator" interface (DEC-13 rule 3: no
  dormant switches). The condition for reopening it is in 4.3.
- No cross-session graph store or dedicated index. `MemoryRecord.links` exists; no measurement
  supports another layer on top of it.
- No `supersedes` revision chain in the store. The old content is in the `llm_completed.tool_calls`
  of the session that proposed it; 4.1 makes it findable.

Tasks:

- [x] 4.1 **Carry evidence with a promotion (R).** (2026-11-16, done before stage 1: it needs only
  the `note_evidence` method, not notes as items) `plan_memory_write` resolves `evidence_refs`
  with the same logic as `note_evidence` and records the resolved item ids and the proposing turn
  in a new optional field on `CanonicalMemoryWrite`; the `memory_written` event carries the same
  field. References that do not resolve are kept as they are; the write is not refused.
  - Nothing model-visible changes: tool schemas, tool results and renders are untouched.
  - Verify first: whether `llm_completed.tool_calls` carries full arguments; whether the four SDKs'
    memory stores keep `provenance` as given (Node's `agent-facade.ts:202` writes empty
    `evidence_refs`).
  - Acceptance: the render part of kernel goldens unchanged byte for byte; a test goes from a
    memory, through `memory_written`, to the proposing `llm_completed`, and on to the tool results
    it cited; old session logs and old memory records still load.
  - This is an additive change to a wire type and needs the §7 exception; see §8 item 5.
  - **Record.** Verified: `llm_completed.tool_calls` carries full arguments; Python and Rust
    already set `provenance.session_id` on the record, Node and WASM did not, so from a stored
    memory one could not even tell which session it came from.
    - Smaller than planned: no new wire field and no change to the `memory_written` event. In
      `plan_memory_write` the kernel replaces the model's `evidence_refs` with the full list: the
      model's references as written, then `call:<id>` for each tool result they resolve to (unless
      the model already wrote that call id), then `proposal:<call id>` for the proposing call
      (`ContextManager::memory_evidence_refs`, sharing `evidence_results` with `note_evidence`).
      All four SDKs already copy that field as given, so only Node and WASM needed `session_id`.
      References that resolve to nothing are kept as written; nothing is refused.
    - From a record: `session_id` → that session's log; `proposal:` → the `llm_completed` whose
      `tool_calls` hold that id; `call:` → the `tool_completed` whose `results` hold that id.
    - Tests: kernel `a_memory_proposal_carries_resolved_evidence_and_its_own_call`; Node end to end
      `leads from a stored memory back to the turn that proposed it and the result it rested on`
      (finds the proposing turn and the cited result in the session log by id). Nothing
      model-visible changes; no golden changed.
- [x] 4.2 **Measure memory upkeep (read-only).** Not in the default gate.
  - The benchmark harness (`memoryTools` in `trial.mjs`) records `memory_tools` on every trial from
    the `llm_completed` events: calls to `note` / `propose_memory` / `memory`, model turns, turns
    that called nothing but those three, the argument characters of those calls, and all characters
    the model wrote. Every benchmark gets it.
  - `deepstrike-lab` gains four metrics: `recall_tokens` (tokens of recalled index lines, from the
    lifecycle `items`, so old results have it too), `memory_calls`, `memory_turn_share` (share of
    turns spent only on memory upkeep), `memory_output_share` (share of what the model wrote that
    went into memory calls). The last three show "–" on old trials, not 0.
  - Change from the original definition, which was "share of input and output tokens": tool
    arguments are resent with history on every later request, so attributing tokens would need a
    per-request split that cannot be done cleanly. Turn share and output-character share measure
    "how much effort the model spends on memory" directly.
  - **Rescoring existing results (`results/multi-session.jsonl`, 6 sequences per session index):**

    | Session index | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
    |---|---|---|---|---|---|---|---|---|
    | `notes`: recall tokens | 0 | 58 | 93 | 123 | 137 | 198 | 202 | 184 |
    | `notes`: proposals accepted so far | 0 | 1.0 | 1.7 | 2.2 | 2.7 | 3.7 | 4.8 | 5.7 |
    | `consolidate` (deleted): recall tokens | 68 | 608 | 738 | 784 | 836 | 768 | 804 | 792 |
    | `consolidate` (deleted): proposals accepted so far | 0 | 4.3 | 9.7 | 14.2 | 20.2 | 26.0 | 34.0 | 40.5 |

    - Injected tokens level off after session 5 (about 200) and do not grow linearly with the
      number of sessions; the context-first plan's stage 9 acceptance criterion holds.
    - Each sequence has only two rules, yet by session 7 `notes` had accepted 5.7 proposals, and the
      model still proposed in every session (0.5–1.2). Old results counted acceptances only, so
      overwrites could not be told from new records; 4.3's `overwrites` and `records` tell them
      apart.
    - The consolidation run pushed proposals to 40 and injection to about 800 tokens with no change
      in success: another side of why it was deleted.
- [x] 4.3 **A drift variant of multi-session (benchmark).** Implemented and measured (2026-11-15).
  - `multi-session.mjs --drift K`: from session index K (0-based, as in task ids) on, the
    sequence's second rule is replaced by one the sequence never had. Nothing announces it; the
    feedback of `submit` is unchanged. Sessions before the drift are the same tasks as without
    `--drift`, so they pair with earlier results.
  - Oracles: `overwrites` (accepted proposals that replaced a record of the same kind and name;
    reported in every mode) and `stale_applied` (after the drift, the answer equals the count under
    the old rules). Each trial keeps the records it started with (`records`).
  - Known limitation: a wrong answer's feedback shows lines that should not have counted, never
    lines the old rule wrongly excluded. Recovery after the drift is harder for it.
  - **Measured:** `--memory notes --drift 4`, 10 sequences, 80 sessions, `deepseek-flash`, the
    working-tree kernel (stage 0). Results in `results/multi-session-drift.jsonl`, variant
    `phase0-multi-notes-drift4`. Usage 11.28M input tokens (8.53M from cache) and 0.90M output
    tokens, about 45% above the estimate (7.7M / 0.69M).

    | Session index | 0 | 1 | 2 | 3 | 4 (drift) | 5 | 6 | 7 |
    |---|---|---|---|---|---|---|---|---|
    | correct / 10 | 2 | 6 | 6 | 7 | 0 | 2 | 2 | 5 |
    | `stale_applied` | – | – | – | – | 0.9 | 0.6 | 0.5 | 0.1 |
    | records at session start | 0 | 0.9 | 1.7 | 2.3 | 2.8 | 3.4 | 4.2 | 4.9 |
    | proposals / of which overwrites | 0.9 / 0 | 0.8 / 0 | 0.7 / 0.1 | 0.7 / 0.2 | 1.0 / 0.4 | 1.1 / 0.3 | 1.0 / 0.3 | 1.0 / 0.1 |
    | `memory` calls | 11.1 | 4.0 | 6.6 | 2.0 | 1.7 | 3.3 | 2.8 | 2.3 |
    | recall tokens | 0 | 50 | 109 | 147 | 175 | 216 | 268 | 293 |

    1. **No recovery within three sessions of the drift.** 7/10 before, 0/10 at the drift (9 of
       those answers were exactly the old rules' count), then 2, 2, 5.
    2. **The model stops applying the old rule but does not learn the new one.** `stale_applied`
       falls from 0.9 to 0.1 in three sessions while success only returns to 5/10. The main cause
       of failure is not stale knowledge but records that contradict each other.
    3. **Memory only grows.** Of 72 accepted proposals only 14 (19%) overwrote a record; the rest
       took new names. By the last session each sequence had 3–7 records about the same thing with
       opposite conclusions (e.g. "exclude only canary" beside "exclude only synthetic-probe"), plus
       invented rules ("estimate 0.75 × the raw count", "deduplicate", "the 00:00 line of each file
       does not count"). It was already so before the drift; the drift made it worse.
    4. **The cause is how the kernel presents memory, not the model.** `propose_memory` says
       "search first, reuse an existing name to update", but the model never sees a name: a recall
       index line with a description shows only `record_id`, `kind` and the description
       (`recall_index_line` in `manager.rs`), and search and point-read results are
       `[MEMORY record_ref=… kind=…] <content>` (`driver/effects.rs`). The 19% were models that
       happened to pick the same slug.
    5. **Memory upkeep took 37.5% of model turns** (`memory_turn_share`: 332 of 885 turns called
       only memory tools). In session 0 the store is empty, yet the model searched 11.1 times on
       average. `memory_output_share` is 62.5%, but this task's answer is a single number, so that
       ratio says little by itself.
    6. **Discarded oracles.** The original `stale_records` and `records_conflicting` judged by
       keyword whether a record mentions a rule. In the live run almost every record mentions every
       rule ("canary lines DO count"); keywords cannot tell affirmation from negation, the values
       are meaningless, and both were removed from the benchmark. Do not read those two fields in
       the result file.
    7. **A weakness of the benchmark.** The `retry` rule (request id ending in 0) was never
       mentioned by any record in 10 sequences: the model cannot see it from three example lines.
       SEQ#6, which starts with it, got all eight sessions wrong; the 5 sequences that drift to it
       got 4 of 15 post-drift sessions right, the other 5 sequences 5 of 15. Too small a sample to
       say `retry` held recovery back, but later experiments should keep it in mind.
    8. **Protocol errors, 2 / 80:** `OpenAI-compatible replay has assistant tool_calls with no tool
       result` (SEQ#2@1, SEQ#3@5). The 0.2.82 `notes` results had 1 / 48 too, so stage 0 did not
       introduce it. Same class of error as the "memory hits inside a tool batch" fixed in 0.2.80;
       investigated separately (see 4.4).
- [x] 4.4 **Decide from 4.3 (G).** The name in memory headers is now the default; the defects are
  resolved in §10.
  - 4.3 landed on "the model cannot repair its memory", but the planned candidate (letting the
    model retire a record) is not the first step: the model cannot even use the existing update
    path, because it never sees a name.
  - **Candidate: show the model the name.** The headers of recall index lines, search results and
    point reads carry `name=<slug>`. No new tool, action or effect. It changes what the model sees,
    so it goes through the DEC-13 gate: behind an experiment switch off by default, measured
    against the configuration without it.
    - Scenarios: multi-session `--drift 4` (the target: success, overwrite share, record count)
      and multi-session without drift (must not get worse). 10 sequences each for a direction,
      then 30+ pairs.
    - Each session's index lines grow by a few tokens; that is cost, judged by `cost_per_success`.
  - **Measured (2026-11-15 to 16).** Experiment switch `DEEPSTRIKE_EXP_MEMORY_NAME=1`
    (`ContextManager::exp_memory_name`, an environment variable, experiment-only, never released).
    Both runs used the kernel before the protocol-error fix, matching their baselines. Results:
    `exp-name-drift4` in `results/multi-session-drift.jsonl`, `exp-name` in
    `results/multi-session-name.jsonl`.

    | | drift: baseline | drift: with name | no drift: baseline (0.2.82) | no drift: with name |
    |---|---|---|---|---|
    | sessions (sequences) | 80 (10) | 80 (10) | 48 (6) | 48 (6) |
    | correct | 30 | 37 | 33 | 33 |
    | of which after the drift (sessions 4–7) | 9 / 40 | 14 / 40 | – | – |
    | proposals / of which overwrites | 72 / 14 (19%) | 74 / 61 (82%) | 39 / 0 | 32 / 25 (78%) |
    | records at the start of the last session | 4.9 | 1.3 | 5.7 | 1.2 |
    | recall tokens in the last session | 293 | 111 | 184 | 101 |
    | `weighted_cost` (per session, relative units) | 90277 | 80265 (−11%) | 73231 | 65873 (−10%) |
    | input / output tokens (per session) | 141k / 11.3k | 119k / 9.4k | 96k / 8.6k | 97k / 6.9k |
    | share of turns spent only on memory | 37.5% | 34.6% | – (not recorded on old trials) | 30.6% |
    | run errors | 2 | 4 | 1 | 1 |

    - **The mechanism changed unambiguously:** with the name in view, the model uses it to update.
      Overwrites went from 19% to 82%, the store from about 5 contradictory records per sequence to
      1, and injection no longer grows with the number of sessions.
    - **Gate** (`deepstrike-lab gate`, DEC-13 defaults): PASS on drift (one metric significantly
      better, none significantly worse). Without drift, FAIL under the default rule ("none
      significantly better", 33/48 vs 33/48); it is not the target scenario, and the "not worse"
      check (`--improve none`) PASSES.
    - **Weaknesses of the evidence, recorded as they are:**
      1. The 80 pairs come from 10 sequences; sessions within a sequence are not independent
         (memory accumulates). The paired bootstrap treats them as independent, so its intervals
         are too narrow. By sequence: 7 better, 2 worse, 1 even. 9.4 counted by session too, so the
         measure is consistent, but the weakness is in the measure, not new here.
      2. Recovery after the drift is still poor: 14 / 40. The name fixes "records only grow", not
         "the model cannot learn a new rule from three example lines".
      3. The no-drift baseline is the 0.2.82 kernel's old result, on the grounds that stage 0
         changes nothing the model sees.
      4. All 4 of the candidate's run errors were the protocol error since fixed, and all count as
         candidate failures, so the conclusion is conservative.
    - **Verdict: passed, now the default (confirmed by the user 2026-11-16).** The environment
      switch is gone; the three headers always carry `name=` (`ContextManager::memory_name_field`;
      a record without a name renders without it); the `memory` tool's description shows the new
      header. None of the four SDKs builds these headers itself; only tests asserting the old
      format changed (3 in the kernel, 2 in the Rust SDK, 3 Node files, 2 WASM, 3 Python). No kernel
      golden changed. Recorded in the CHANGELOG and `MIGRATION-0.2.83-to-0.2.84.md`.
  - Only if records still only grow once the model can see names is a retire action considered;
    offline consolidation comes after that, and must be measured on the drift variant.
  - **The protocol errors (2026-11-16).** Reproduced offline with a scripted provider (random
    batches, 80 runs, plus about 40 targeted cases); all four are now fixed, see §10.2:
    - the `propose_memory` outcome written between an assistant's calls and their results (the
      live error);
    - `memory` + `propose_memory` in one turn failing the run in all four SDK runners;
    - the repeat fuse calling the provider while a memory query of the same turn was outstanding;
    - a call to an unexposed kernel tool ending the run with `invalid_authority`.
  - Noted, outside this task: point 5 above (repeated searches of an empty store). Look at the
    numbers again after the name change before acting.
- [ ] 4.5 **Candidate experiment, unscheduled: searching raw transcripts as a model tool.**
  `transcript_search` was only ever measured as part of the consolidation run, never as "the model
  searches its own past logs during a session". Decide whether it is worth a measurement once
  4.3's benchmark is in place.

## 6. Order and releases

```text
stage 0 ──► stage 1 (1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.6 → 1.7 → 1.8 → 1.9) ──► stage 3
              └──► stage 2 can start after 1.4 (its switches need the unified downgrade path)

stage 4: 4.2 and 4.3 do not depend on stage 1 and can start any time; 4.1 after 1.2 (once notes
are items, the two evidence resolvers become one); 4.4 after 4.3
```

- Stages 0 and 1 change nothing the model sees and can ship as one release. So do 4.1 and 4.2.
- Stage 2's deletions change renders and ship as a release of their own, with a MIGRATION note. So
  would a retire action from 4.4.
- The release rule is unchanged: the release PR merges to main first, then the merge commit is
  tagged.

## 7. Constraints

- Every clause of DEC-13 still holds. This plan adds no effect, origin or relation. The only new
  lease kind allowed is 1.1's, and only as a replacement for `SingleUse`.
- Any PR in stages 0–1 that changes the render part of a golden is not a refactor: stop and find
  out why; do not bless.
- No unification for its own sake. Two concepts merge only if they answer the same question;
  `CapabilityLease`, `MemoryScope` and `RepeatFuse` belong to effect admission and stay where they
  are.
- No change to the ABI types, journal or restore of `runtime/kernel/wire` (the one exception is
  4.1's optional field, approved by the user; §8 item 5). `runtime/` has about 31k non-test lines,
  the largest part of core, and deserves an audit of its own outside this plan.
- The kernel reads no clock and initiates no model call (D1, DEC-13 rule 1).

## 8. Decisions for the user

1. The spending cap for stage 2's measurements.
2. Whether `EntropyWatchConfig.notify_model` (off by default) is deleted under "no dormant
   switches". It is a second channel beside the host supervisor, with no measurement behind it.
3. The conflicts between today's behavior and the write-authority table that 1.6 lists, one by one.
4. Stage 1 changes the checkpoint shape. Proceeding under the earlier agreement ("breaking changes
   allowed, with a MIGRATION note"); say so if stronger compatibility than restoring old
   checkpoints is needed.
5. ~~4.1 adds optional fields to `CanonicalMemoryWrite` and the `memory_written` event, touching the
   §7 constraint "no change to wire ABI types".~~ Decided by the user 2026-11-15: approved. 4.1
   comes after 1.2.
6. The spending cap for 4.3's measurements (separate from item 1).

## 9. Working notes

- Environment: see the handoff section of the context-first plan. Rebuild the napi addon and
  `node/dist` before running Node tests or benchmarks; `.env` needs `DEEPSEEK_API_KEY` and
  `DEEPSEEK_MODEL`; WASM tests need the rustup toolchain; Python uses its own venv.
- Regenerating goldens: `BLESS_KERNEL_RECORD_FIXTURES=1 cargo test -p deepstrike-core --lib
  <test name>`. Stages 0–1 should not need it in principle.
- Every PR runs `cargo test --workspace`, the Node / Python / WASM tests and conformance in all four
  SDKs.
- Baselines reproducing pre-0.2.82 behavior must pass `notesBudgetTokens: 0` explicitly.
- Gate: `deepstrike-lab gate` without arguments applies DEC-13.
- Verify against the code every conclusion §2 marks "to verify" and every "unused" claim before
  acting; where the code disagrees with this plan, the code wins and the plan is updated.

## 10. Handoff (2026-11-16)

Whoever picks this up starts here. Branch `refactor/kernel-distill-phase0`, based on `main`
(`df26eabf`, v0.2.83); not pushed, no PR yet.

### 10.1 Done, all on this branch

| Commit subject | Content |
|---|---|
| stage 0 cleanup | 0.1–0.7 of stage 0: removes `asyncSummarizer`, `summary_upgraded` and unread configuration; one compactor instead of two |
| benchmarks and lab | the four memory upkeep metrics of 4.2; `multi-session --drift` of 4.3; the result files |
| protocol fixes | the order of the `propose_memory` outcome (kernel); `memory` + `propose_memory` in one turn (Node runner); the reproduction script `scripts/kernel-tool-batches.mjs` |
| name in memory headers | 4.4, on by default; CHANGELOG, MIGRATION, docs |
| the plan | this document |

### 10.2 To do, in order

1. **Two defects.**
   - [x] The repeat fuse beside `memory` in one turn (2026-11-16, `dfcb62fb`). Root cause: when the
     fuse or a gate answered a turn's host calls with synthetic results, the engine called the
     provider at once, whether or not the same turn's syscalls had published kernel work (a memory
     query or write); one step published `QueryMemory` and `CallProvider` together, and the next
     request raced the query and was rendered without its result. Fix: in that case the synthetic
     commit returns `AwaitingResume`, and the turn continues through `resume_after_preload` once the
     work settles, as an unfused turn does (`turn_awaits_kernel_work` / `defer_provider_call` in
     `state_machine/mod.rs`, `commit_synthetic_results` in `gate.rs`). Kernel regression test
     `a_fused_turn_waits_for_the_memory_query_it_made`, which fails without the fix.
   - [x] **An unexposed kernel tool ended the run** (2026-11-16, `dd6d11e4`). The user's decision:
     "First ask why the kernel tool was not exposed — that is the core. Calling a tool that does not
     exist should return an error, so the model goes looking for the real tool; after repeated
     failures, move on to the summary."
     - **Why it was not exposed.** `read_result` was offered only while some tool result was out of
       context. A page-in brought the result back, and so did `reset_collapse_generation` at a
       compaction or renewal; once the last such result was back, the tool vanished from the next
       turn's toolset, although the model's history showed it using the tool and its description
       tells it to page through large results with `offset`. The golden
       `golden_lifecycle_external_payload` recorded exactly this: the request after the page-in had
       no `read_result`. Fix: once offered, always offered (`ContextManager::read_result_offered`,
       checkpointed). The toolset no longer shrinks, which also removes one source of cache prefix
       breaks.
     - **Calling a tool it does not have.** The driver no longer ends the run with
       `invalid_authority`; like an unexposed host tool, the call gets an error result ("Tool '…'
       is not available on this turn. Choose from the tools listed in your tools schema.") and is
       not executed. Such calls count toward the repeat fuse, meta-tool names included: denied
       after 5 repeats, and after 8 the run moves to the report turn and ends `NoProgress`. The
       terminate rung's report turn itself offers no tools; a tool call there ends the run
       `NoProgress` as usual.
     - Tests: `a_tool_the_turn_never_exposed_has_no_caller_to_derive` (submit path),
       `repeated_calls_to_an_unexposed_kernel_tool_burn_the_repeat_fuse` and the regenerated
       golden, each failing without the fix. `scripts/kernel-tool-batches.mjs` 117/117; three random
       seeds, 40 runs each, no failure. Full test suites of all four SDKs pass.
     - Both change what the model sees on error paths. They were handled as defect fixes, not
       through the DEC-13 gate, and are recorded in the CHANGELOG and MIGRATION.
2. **Gaps left by the previous round.**
   - [x] A regression test for the Node runner's fallback (`b97c9c13`), failing without the fix.
   - [x] The WASM, Python and Rust runners had the same problem; fixed with the same fallback, each
     with a regression test that fails without the fix (`b3f012b0`).
   - [x] Python: the old venv pointed at a Python 3.11 that no longer exists. A fresh Python 3.12
     venv in a scratch directory plus `maturin develop --release` runs the full pytest suite green
     (913 passed). The local shell sets a SOCKS proxy; clear `ALL_PROXY` / `HTTP(S)_PROXY` before
     running pytest, or httpx complains that `socksio` is missing.
   - [x] This English version, which `node scripts/check-docs-drift.mjs` requires.
3. **Stage 1 (1.1–1.9)**, then **4.1** (after 1.2; the wire optional field is approved).
4. ~~**Stage 2's DEC-13 audit.**~~ First round done, M1–M7 deleted (see stage 2's record). M8 and M9 open.
5. **Stage 3, documentation.**

### 10.3 Lessons of this round, before you start

- **Read the data before concluding; do not conclude from an oracle's number.** Both keyword
  oracles designed for 4.3 were worthless, which only reading the records themselves revealed; it
  is also why 4.4's candidate changed from "a retire action" to "show the name". For every keep or
  delete in stage 2, open a few raw trials.
- **Session-paired samples are not independent.** Multi-session's 80 pairs come from 10 sequences.
  Report wins and losses by sequence as well.
- **Recovery after the drift is still poor** (14 / 40). The name fixed "records only grow", not "the
  model cannot learn the new rule". The `retry` rule (request id ending in 0) was never mentioned by
  any record in any run, so it may be too hard for the benchmark; confirm with the user before
  changing the benchmark, since that makes old and new results incomparable.
- **Memory upkeep takes over 30% of model turns**, and in session 0 the model searches an empty
  store about ten times. Not addressed, only measured (4.2, 4.3). It is the most visible cost
  outside stage 2.
- **Measurements cost more than estimated.** 4.3's first run went about 45% over. Leave a margin
  when asking for a budget, and since the balance cannot be checked from here, ask the user to
  confirm it.

### 10.4 Operational pitfalls

- There is no `napi` CLI on this machine. To rebuild the addon: `cargo build --release -p
  deepstrike-node`, then copy `target/release/libdeepstrike_node.dylib` to
  `crates/deepstrike-node/deepstrike-core.darwin-arm64.node`. If the binding interface
  (`crates/deepstrike-node/src`) changes, this does not regenerate `index.js` / `index.d.ts`;
  install `@napi-rs/cli` v2 for that.
- While a benchmark runs in the background, do not overwrite the `.node` file in place and do not
  run `npm run build` (it deletes `node/dist` first). Replace the addon by copying to a temporary
  name and then `mv`.
- Under zsh, `echo ====X` and an unquoted `--include=*.rs` fail; quote path globs.
- Do not chain other commands after a test command with `&&` for convenience. A previous round ran
  a `git stash` by accident that way.
