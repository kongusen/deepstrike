# Context First: Implementation Plan for the DeepStrike Architecture Shift

> Revised 2026-11-01: breaking changes are allowed. Superseded mechanisms are deleted outright, with
> no plugin, compatibility layer or same-name template; migration notes go into per-version files
> such as `MIGRATION-0.2.77-to-0.2.78.md`.
>
> Revised 2026-11-02: no jump to 0.3.0. The plan ships in three releases, 0.2.78–0.2.80; see
> "Release plan". The originally planned 0.2.81 is folded into 0.2.80, making it a larger release.

## Overview

DeepStrike's core principle shifts from "a verifiable effect kernel" to:

> Under a finite attention budget, decide for every piece of information its **scope, form,
> lifetime and write authority**, and let those decisions be **corrected by outcomes**; side
> effects are constrained separately by **effect admission**.

Current problems (checked against the code):

| # | Problem | Evidence |
|---|---------|----------|
| Q1 | Lifecycle has no first-class object: recall, skills, dependency outputs and tool results each take their own path | `context/partitions.rs`, the four runners |
| Q2 | Lifecycle policy lives in the host: `prefetchMemory` is written once per runner (14.6k lines across four), and recall placement, dedupe and key rules all live in the host | `node/src/runtime/runner.ts:4064` and the Python/Rust/WASM counterparts |
| Q3 | Knowledge renders before the turns, so a mid-run append invalidates the cache of the whole history, while the comment called it the "cheap direction" | `context/partitions.rs:117` |
| Q4 | Reference detection and eviction rely only on lexical overlap | `partitions.rs:observe_references`, `context/utility.rs` |
| Q5 | With `delegate_compaction` on, the context the model sees and the one the kernel computes pressure on differ | `context/manager.rs:415`, b0dadad0 |
| Q6 | DAG edges are string concatenation cut by characters; the `summary` mode is really head/tail clipping | `workflow-control-flow.ts:28-57` (deleted in 2.2) |
| Q7 | Plan/Implement sub-agents inherit `Full` by default | `types/agent.rs:172` |
| Q8 | Dynamic orchestration has **three** paths: declarative `NodeKind::Loop/Classify/Tournament`, model-time `submit_workflow_nodes`, and the host program `dynamic.ts` (Node only) | `orchestration/workflow/mod.rs:76`, `node/src/workflow/dynamic.ts` |
| Q9 | Memory recall happens only at turn 1 and renewal, with no quality feedback | the `prefetchMemory` call sites in the four runners |
| Q10 | No feedback: no way to know which evictions were wrong or which injections were wasted | — |
| Q11 | Every provider call first `await`s a `context_prepared` write, so the log write blocks the call | `node/src/runtime/runner.ts:3205` |

## Two corrections to the previous draft

1. **Deterministic replay does not need to go.** Kernel restore is "checkpoint + replay the recorded
   tail inputs + compare the state digest" (`runtime/kernel/wire/restore.rs:26`). As long as every
   non-deterministic signal (model relevance judgments, measured usage, model summaries, provider
   compaction) enters the kernel as a **recorded input**, replay holds by construction;
   `UsageFacts` already arrives that way (`wire/event.rs:199`). What has to go is the **context
   evidence layer** stacked on top (`ContextExecutionInput` digest, `context_prepared`, shared
   evidence, verifiable reports, evolution bindings); the restore mechanism stays. The previous
   task 0.2 ("read recorded decisions instead") is therefore cancelled.
2. **Dynamic orchestration has three paths, not two**, and Loop already returns its control signal
   through the `pace` meta-tool. Once Classify/Tournament are deleted, the "parse a JSON control
   signal out of text" problem disappears with them; it needs no separate structuring task.

## Architecture decisions

- **D1 The kernel is a deterministic state machine over recorded inputs.** Every non-deterministic
  signal enters the journal first, as an effect result or a fact, and only then drives a kernel
  decision. The kernel never calls a model or reads a clock itself.
- **D2 Delete the context evidence layer.** Not turned into a plugin: deleted (list below). Crash
  recovery depends only on journal + checkpoint.
- **D3 There is one context object: `ContextItem`.** The four partitions stop storing data and become
  render positions derived from item attributes.
- **D4 Policy in the kernel, I/O in the host.** "Where it goes, how long it stays, what form it
  takes" is written once, in the kernel; the host only fetches data (memory store, payloads,
  providers).
- **D5 One authority per context fact.** When a provider rewrites the context the model sees, the
  kernel absorbs the rewrite as a fact instead of maintaining a fictional full history.
- **D6 Dynamic orchestration = host program or model submission + kernel admission.** The kernel
  keeps only `Spawn` and `Reduce` nodes, plus edges, quotas and cancellation.
- **D7 Effect admission is unchanged.** Governance, approval, sandbox and quotas are a separate line
  of work that this plan does not touch.

## Core model: ContextItem

```text
ContextItem {
  id / key       stable identity; a keyed item upserts by key
  origin         system | skill | memory_recall | tool_result | dependency_output
                 | user | assistant | task_state | signal | provider_summary
  scope          operation | task | edge(from→to) | session | agent
  fidelity       full | structured | handle(locator, preview)
  lease          permanent | until_boundary | turns(n) | until(event) | single_use
  authority      host | kernel | model | provider
  value          { injected_at, last_referenced, ref_count, paged_in_after_evict, judged_relevance? }
  cost           { tokens(measured|estimated), cache_position }
}
```

`volatility` is no longer stored; it is derived from origin + lease. `summary` is no longer a
separate fidelity (see task 3.1).

**Render position rules:**

| Derived volatility | Render position | When it may change |
|--------------------|-----------------|--------------------|
| stable | system[0] | only at operation start |
| durable | system[1] (knowledge) | only at a boundary (renewal/compaction) or while history is empty |
| episodic | history | appended to the tail at any time; rewrites go through the compression pipeline |
| volatile | state_turn | rebuilt every turn |

**Cache invariant (fixes Q3):** a durable item that arrives while history is non-empty is first
appended to the tail of history as an episodic item and promoted into system[1] at the next
boundary. The only exception is a caller that explicitly declares `urgent`; the kernel then records
a `cache_invalidation{tokens_at_risk}`. Skill activation, renewal recall and host-pinned material
all follow this rule.

## Tasks

### Stage 0: delete the context evidence layer (fixes Q11, unblocks later stages)

- [x] Task 0.1: delete the evidence layer on the run path (2026-11-01, branch
  `feat/context-lifecycle-stage0`)
  - Deleted: `context/execution.rs`, `evolution.rs`; `CallProviderEffect.context_candidate`,
    `OperationConfig.artifact_set_binding`, checkpoint `state_generation`; the `context_prepared`
    event; the three bindings' `contextPrepareJson`/`contextVerifyJson`/`evolutionValidateJson`;
    the four SDKs' shared evidence, evolution and context preparation modules and the
    `artifactSetDigest` option; the conformance domains `context_execution` and
    `shared_evidence_snapshot`.
  - Side benefit: the kernel no longer renders the context a second time per provider call to build
    the candidate.
  - Verified: Rust workspace, Node (1422), Python (912), WASM (jest 296 + the real WASM kernel
    regression) and four-SDK conformance (23 fixtures) all pass. The restore state-digest self-check
    stays on.
- [x] Task 0.2: delete the offline evidence tools (2026-11-01). Restore already checks the chain,
  replays deterministically and self-checks the state digest, so chain_validator, verifiable.rs,
  the `deepstrike` CLI, `verifiableOperationJson` and the SDK verifiable reports are all deleted.
- ~~Original task 0.2: trim the offline evidence tools~~
  - `runtime/verifiable.rs`, `runtime/chain_validator/`, the `deepstrike` CLI and the SDK
    `verifiableOperationJson`/verifiable-report are not on the run path and were kept by 0.1 for
    now. Open: shrink chain_validator to restore regression tests (keeping only C1 chain integrity
    and C3 replay determinism) and delete the SessionLog input plane and the CLI `verify` command.
  - Verify: restore at three crash points (after compaction, after recall, after renewal) and get
    the same context as before the crash.

### Stage 1: one ContextItem model (fixes Q1, Q3)

- [x] Task 1.1a (2026-11-02): new `context/item.rs` with `ContextItem` (origin / lease / value) and
  the origin table (`ItemOrigin::defaults`: scope / fidelity / lease / authority for each of 11
  origins). Knowledge partition entries are `ContextItem`s and `KnowledgeEntry` is deleted.
  Host-written knowledge takes its origin from its key (`skill:` → skill, anything else →
  reference); kernel recall is `memory_recall`, the governance note is `system`. Four leases:
  permanent, until_boundary, turns(n), single_use; an expired item is marked to leave at the next
  boundary (system[1] is never rewritten inside a cache generation). A recall index line defaults
  to `turns(8)`, counted from its last recall or reference; reading it with `memory(record_id)`
  counts as a reference and marks it `expanded`. `lease_remaining_steps` is computed from the
  lease. Checkpoint knowledge slots carry origin, lease, leave reason and value, and
  `retired_items` records items that left; a slot without an origin infers it from its key
  (`memory:` → recall). Not done: the `until(event)` lease; scope / fidelity / authority are only
  table entries so far, with no behavior.
- [x] Task 1.1b (2026-11-02, partial): every history message carries an
  `ItemTag { id, origin, injected_turn }`. Ids are allocated in arrival order, never reused, and
  survive compaction archival, renewal carry-over and checkpoints (`history_items`,
  `last_item_id`); a checkpoint without `history_items` restores ids as 0. Origin follows the role
  (user / assistant / tool_result / system). Handles are still anchored to their tool_result item by
  call_id, and a handle's residency is that item's fidelity (Resident = full, Collapsed / External /
  PagedOut = handle). `read_result` on an earlier tool result or a `dep:` dependency output counts
  as a read-back. The lifecycle report's `history` tallies by origin: items and tokens that entered
  history, those still there at the end, and read-backs.
  - 0.2.79 first item (2026-11-02): the system[0] and history `Partition` is an item table. Each
    row is one item (its `ItemTag`, rendered message and host measurement), stored by column with
    private fields and written only row by row; the renderer and the compression planners read the
    message column as a read-only slice. Compaction acts on items: an item snip or micro rewrites in
    place keeps its id and records the new `excerpt` fidelity (the checkpoint's `history_items`
    writes the field only when it is not full); collapse and auto take whole rows out, and the
    archived result carries the item tags. Fixed along the way: rollback used to truncate only the
    message column, leaving measurements, tags and the token count misaligned; it now truncates
    whole rows.
  - Remaining 0.2.79 items (2026-11-02; task 1.1 complete):
    - `until(event)` lease: a new `LeaseEvent`; a skill body's lease is `until(skill_deactivated)`,
      so it leaves when its skill is deactivated (at the next boundary). A pin extends only
      time-limited leases (turns, boundary, single use), never an event lease.
    - Authority: a host `seed_knowledge` / `apply_knowledge_mutation` may not overwrite or remove
      an item the kernel or the provider wrote, nor remove the body of an active skill; such a
      command is refused whole with `InvalidAuthority`.
    - Scope: `ItemScope::inherited_by` decides which parent knowledge a child run starts with
      (`full` inherits operation / task / session / agent items, `system_only` only agent items,
      edge items never); items on an event lease and kernel-written items are not inherited. The
      kernel sends them as `inherited_knowledge` in the spawn metadata, and Node / Python / WASM
      pass them to the child run. A renewal keeps every scope but edge (`survives_renewal`).
    - Handles in the item table: a handle carries the id of its history item (checkpoint
      `HandleState.item`, backfilled by call_id for older checkpoints) and is pruned by item id.
      Fidelity decides rendering: an item whose body is collapsed or paged out renders as its
      handle preview (`ContextManager::history_fidelity`).
    - Per-item history detail: the lifecycle report's `history_items` lists the latest 128
      history items (id, origin, tokens, the fidelity they rendered, and why they left:
      `compacted` / `renewed` / `removed`), older ones count in `history_items_omitted`; retired
      records are checkpointed (`retired_history`).
    - Migration notes: `MIGRATION-0.2.78-to-0.2.79.md`.
- ~~Original task 1.1: introduce `ContextItem`, the item table and the origin table in the kernel~~
  - Acceptance: `ContextPartitions` becomes a render view over the item table; `KnowledgeEntry`,
    history messages and handles all map to items and the old types are deleted. One table defines
    each origin's default scope/fidelity/lease/authority (for example `memory_recall → session /
    handle(index line) / turns(N) / host`), and the host has no API that writes a partition
    directly. `lease_remaining_steps` in `mm/value.rs`, always `None`, is computed from the lease.
  - Verify: the existing renderer and compression tests pass after moving to the new model; every
    lease has an expiry test.
  - Files: new `context/item.rs`; changes to `context/partitions.rs`, `manager.rs`, `renderer.rs`,
    `mm/handle.rs`, `mm/value.rs`

- [x] Task 1.2 (2026-11-01): knowledge that arrives after the prefix was sent renders in the
  state_turn first (staged) and is promoted at the boundary; once staged tokens reach history/4 it
  is promoted at once and counted in `cache_invalidated_tokens`. `prefix_sent` and `staged` are in
  the checkpoint.
- ~~Original task 1.2: implement the render position rules and the cache invariant~~
  - Acceptance: activating a skill mid-run does not shorten `PrefixFingerprint.common_turn_prefix`
    and the promotion at the boundary succeeds; fix the comment at `partitions.rs:117` and
    `docs/concepts/prompt-cache-design.md`.
  - Files: `context/manager.rs`, `renderer.rs`, `partitions.rs`

### Stage 2: move lifecycle policy into the kernel (fixes Q2, Q9)

> Prerequisite: the uncommitted recall changes on branch `feat/xiaoman-memory-policy-v2` are exactly
> the host-side logic this stage deletes. Decide whether to merge or drop them before starting, so
> the two do not change the same code.

- [x] Task 2.1 (2026-11-02): with `memory_policy.recall` on, the kernel issues `query_memory`
  (causation `kernel_recall`) with the goal as the query at run start and after every renewal;
  hits land as `memory:<record_id>` knowledge index lines (dedupe in the kernel). The four SDKs
  drop prefetch and `memoryRecallTurn`; the host only runs the search, and `preQueryMemory` stays as
  the query rewrite hook. Not done: recall when `task_state.plan` changes; child nodes are covered
  by recall at the start of their own run.
- ~~Original task 2.1: move recall into the kernel and add recall points~~
  - Acceptance: the kernel issues `memory_query` effects at four points — turn 1, renewal, a
    `task_state.plan` change and child spawn — with the current goal or sub-goal as the query; the
    host only runs the query and returns results. Index line generation, dedupe by
    `memory:<record_id>`, placement and lease are kernel decisions. Delete the four runners'
    `prefetchMemory` and the three `recall-projection.*`. The `preQueryMemory` hook stays as the
    query rewrite hook.
  - Verify: the recall test vectors run in kernel unit tests; SDK tests cover I/O only.
  - Files: new `context/recall.rs`

- [x] Task 2.2 (2026-11-01): the kernel composes the dependency section from each node's
  `DependencyContext { mode: full | reference | none, max_tokens }` and sends it with the spawn as
  `dependency_brief`; the three SDKs' `workflow-control-flow.*`, the `summary` mode and the
  never-effective `include` are deleted. The Rust SDK carries dependency outputs for the first
  time. The budget note stays in the host (it depends on the host's remaining budget).
- ~~Original task 2.2: the kernel writes child briefs~~
  - Acceptance: goal, dependency outputs, budget note and loop instructions are generated by the
    kernel as an item list sent with the spawn; delete Node/Python/WASM `workflow-control-flow.*`
    (the Rust SDK has no such file).
  - Files: `orchestration/workflow/run.rs` (`spawn_info`)

### Stage 3: first-class DAG edges (fixes Q6, Q7)

- [x] Task 3.1 (2026-11-02): a child run's kernel holds its dependency outputs through
  `initial_context.dependencies`, and `read_result(call_id="dep:<agent_id>")` is answered by the
  kernel directly (no host effect, no payload store). A clipped brief says how to read the rest; a
  new `handle` mode shows only a 600-character preview. The four SDKs pass the spawn's
  `dependency_outputs` into the child run (the Rust SDK only has the child side; its workflow
  orchestration belongs to 6.3). Not done: structured `fields` slices by output schema. The default
  mode is now `handle` (2026-11-02, based on the 4.3 step-two rerun).
- ~~Original task 3.1: `EdgeSpec` and structured node outputs~~
  - Acceptance: `depends_on` takes `{ mode: full | handle, max_tokens, fields? }`, replacing the
    current `full | summary | reference`. Node outputs are stored as handle items (`scope = edge`).
    Default `handle`: downstream sees only the preview and pages in the full text with
    `read_dependency(id, range|field)`. `fields` gives structured slices together with the node's
    output schema. All budgets are in tokens. Delete `summarizeDependency` and the `summary` mode:
    when compression is needed, the upstream node returns a compact structure per its output
    schema; the edge does not summarize.
  - Verify: for fan-out(5)→synthesize, the downstream initial context is at most Σ preview; page-in
    returns the full output.
  - Files: `orchestration/workflow/mod.rs`, `run.rs`, `mm/handle.rs`; SDK-side `read_dependency`
    meta-tool

- [x] Task 3.2 (2026-11-01): Plan/Implement default to SystemOnly (two kernel default tables +
  three SDK templates). Not done: recording the token cost when Full is turned on.
- ~~Original task 3.2: fix the inheritance defaults~~
  - Acceptance: Plan/Implement default to `SystemOnly + brief` (the brief comes from the parent's
    task_state and explicit handoff); `Full` is opt-in only, and its token cost is recorded.
  - Verify: child nodes of the default workflow start with clearly fewer tokens, and success rate on
    the stage 4 benchmark does not drop.
  - Files: `types/agent.rs:169`, `proc/mod.rs`

### Stage 4: feedback loop (fixes Q4, Q10)

- [x] Task 4.1 (2026-11-01, summary part): at the end of a run the kernel emits a `lifecycle_report`
  observation (knowledge entries, referenced count, regret, waste_tokens, cache_loss_tokens), which
  the four SDKs project to the session event `context_lifecycle_reported`; evicted keys, regret and
  waste are in the checkpoint. Per-item detail and recall hit rate (2026-11-02, with 1.1a):
  `lifecycle_report` adds `recalled`, `recall_expanded` and `items` (each knowledge item's key,
  origin, tokens, injection turn, reference count, whether it was expanded, whether it came back
  after removal, and why it left: evicted / expired / removed), projected verbatim onto
  `context_lifecycle_reported` by all four SDKs. History item detail comes with 1.1b.
- ~~Original task 4.1: lifecycle report~~
  - Acceptance: every run reports each item's injection, references, eviction, page-in and expiry,
    plus three metrics:
    - **regret**: items paged in or requested again after being evicted or collapsed;
    - **waste**: tokens never referenced between injection and removal;
    - **cache_loss**: cached tokens lost to prefix rewrites.
    The recall hit rate (share expanded with `memory(record_id)`) and waste are in the same report.
  - Files: new `context/lifecycle_report.rs`; SDK `on_turn_metrics` adds the summary

- [x] Task 4.2 (2026-11-03, partial): reference evidence in grades — strong (`memory(record_id)`, the key
  or record id in tool arguments, the key named in text) and lexical (distinctive terms only: common
  words and the index-line header removed); `ItemValue` records `strong_refs` / `lexical_refs`, and
  retention weighs a strong reference as four lexical ones. Not done: the model `RelevanceJudge`
  (an effect) — neither benchmark reaches knowledge-budget eviction, so the lab cannot show its
  value; it waits for a scenario with budget pressure.
- ~~Original task 4.2: reference evidence and relevance judgment~~
  - Acceptance: reference evidence has three strengths: the model explicitly calls a handle or
    `memory(record_id)` > the item's key appears in tool call arguments > lexical overlap.
    `RelevanceJudge` has a lexical (fallback) and a model implementation; the model one is an
    effect that scores in batches and only fires above a pressure threshold, and its result is
    written as a fact into `value.judged_relevance` (D1). Utility scoring and knowledge eviction
    both read that value.
  - Verify: on a fixed long-task benchmark, regret and waste are no higher than the lexical-only
    version and success rate does not drop.
  - Files: `context/utility.rs`, `context/partitions.rs`, new `context/relevance.rs`

- [x] Task 4.3 (2026-11-03): `crates/deepstrike-lab` (`report` / `gate`) and two live benchmarks
  (`long-task.mjs` ledger-audit, `live.mjs` fan-out) writing one trial per line: success, cost, the
  kernel estimate next to the provider count of the same call, and the lifecycle reports.
  0.2.80 results (2026-11-03, DeepSeek `deepseek-flash`, baseline 0.2.79 + the memory fix `30e93db5`,
  `benchmark/context-lifecycle/results/`):

  | Scenario | Variant | Correct | Input tokens / task | Estimate error (size-weighted) |
  |----------|---------|---------|---------------------|--------------------------------|
  | ledger-audit | 0.2.79 → 0.2.80 | 11/12 → 11/12 | 52.4k → 56.5k | 11.6% → 0.7% |
  | fan-out full | 0.2.79 → 0.2.80 | 12/12 → 12/12 | 24.3k → 24.0k | 8.4% → 4.7% |
  | fan-out handle | 0.2.79 → 0.2.80 | 12/12 → 12/12 | 10.1k → 12.5k | 8.0% → 7.4% |

  All three gates (success not lower, size-weighted estimate error ≤10%) pass. Ledger archives 15.5
  items per task instead of 4.1: replay (reasoning) now counts toward pressure, where the kernel used
  to undercount what the model sees. Waste rising from 2 to 68 tokens is a change in counting
  (common words no longer mark unrelated notes as used) and is not compared. Measuring lesson: a
  metering wrapper that does not forward the provider's other members loses replay and the
  descriptor, so it measures something other than the real path (fixed; earlier results discarded).
- ~~Original task 4.3: offline tuning~~
  - Step one (2026-11-02): `benchmark/context-lifecycle/run.mjs` drives the real kernel with a
    scripted provider and compares the contexts two checkouts render (task success is not measured).
    Against baseline `5b5308d3`: long-task prefix reuse 0.761→0.785, knowledge rewrites under cache
    2→0, total input unchanged; fan-out→synthesize in `handle` mode cuts the first call from 48.5k
    to 4.5k characters (−91%), and `read_result` still reads the full text. The 3.2 inheritance
    default is invisible in this scenario (a workflow root has no parent transcript). Task success
    needs a real provider.
  - Step two (2026-11-02): `benchmark/context-lifecycle/live.mjs` measures fan-out→synthesize answer
    accuracy with a real model. Three explore nodes return fixed 16k-character reports (no model
    call); only synthesize calls the real provider. 12 questions, with the access code inside the
    600-character preview, inside the 8k clip, or beyond it. `--provider deepseek` reads
    `DEEPSEEK_*` from `.env` (`openai` reads `OPENAI_*`; the yinli.one key returns 401 and is not
    usable for now); `--repo` points at the baseline worktree. DeepSeek `deepseek-flash` results
    (after fixing the two problems below):

    | Variant | Correct | Chars sent | Calls | First call |
    |---------|---------|------------|-------|------------|
    | Baseline full (`5b5308d3`) | 11/12 | 593k | 12 | 49.4k |
    | Branch full | 12/12 | 904k | 20 | 25.1k |
    | Branch handle | 9/12 | 699k | 26 | 31.3k |

    All three handle failures were DeepSeek replay errors (see the known issues under "Handoff"),
    not wrong answers. Branch full halves the first call, but the model reads clipped dependencies
    with `read_result`, so it sends more in total than the baseline; accuracy 12/12. The default
    edge mode could not be decided until the replay problem was fixed and handle rerun.
  - Step two rerun (2026-11-02, after fixing both replay problems; 24 questions per variant, 8 per
    access-code depth):

    | Variant | Correct | Chars sent | Calls | In preview / in clip / beyond clip (chars sent) |
    |---------|---------|------------|-------|-------------------------------------------------|
    | Baseline full (`5b5308d3`) | 24/24 | 1185k | 24 | 395k / 395k / 395k |
    | Branch full | 24/24 | 1861k | 41 | 254k / 918k / 689k |
    | Branch handle | 24/24 | 1394k | 51 | 560k / 503k / 331k |

    The replay problem had two sources: the kernel collapsing old narration broke the content-keyed
    reasoning lookup (fixed, see "Handoff"); and `deepseek-flash` really does omit reasoning on some
    tool-call turns (4 of 51 calls here). DeepSeek accepts an empty replayed `reasoning_content` and
    rejects a missing one, so the empty string is now replayed as returned. All three variants are
    equally accurate. Branch full costs 57% more than the baseline: after the 8k clip the model
    almost always reads the full text with `read_result`. Handle is 25% cheaper than branch full,
    but the model still reads the full text when the answer is in the preview. The baseline does
    not clip and is cheapest for 16k outputs, but its downstream initial context grows linearly
    with fan-out width.
  - Acceptance: `crates/deepstrike-lab` provides a tool that compares `ContextPolicy` variants
    (thresholds, lease defaults, edge mode) by lifecycle report and task outcome (success/failure,
    cost). It does not depend on the deleted evolution module.
  - Files: `crates/deepstrike-lab`, `benchmark/`

### Stage 5: one context authority (fixes Q5)

- [x] Task 5.1 (2026-11-03, kernel side): the message ABI gains `item_id` and an opaque `replay` (the
  kernel keeps reasoning / native blocks with the item and renders them back; all four SDKs send and
  prefer it); the rendered request carries `estimated_tokens` (tool schemas and replay included,
  calibrated by the least-squares line through the last eight estimate/count pairs, raising but never
  lowering pressure); `ProviderCompleted` gains `provider_compaction` (a summary through an item
  becomes one `provider_summary` item) and `cleared_tool_results` (rendered as handles), passed
  through by all SDKs from an optional `takeContextEdits` hook. Not done: the Anthropic provider
  reporting either fact (no Anthropic key to test its three compaction flows).
- ~~Original task 5.1: absorb provider rewrites of the context~~
  - Checked (2026-11-01): the kernel message ABI carries plain text only, and native blocks such as
    compaction/thinking stay inside the host provider. Replacing covered history with a
    provider_summary item therefore first needs opaque blocks on `ProviderMessage`/`LogicalMessage`
    and changes to both compaction kinds in the four SDKs. Changing only the renewal criterion gains
    nothing: the renderer drops the oldest history beyond budget at almost the same moment as the
    renewal threshold (0.98). This task depends on 1.1 and that ABI extension.
  - Acceptance: when a provider returns a compaction block, the kernel receives a
    `ProviderCompacted{covered_range, summary_tokens}` fact and replaces the covered history items
    with one `provider_summary` item (`authority = provider`, carrying the native block).
    `clear_tool_uses` likewise: once backfilled as a fact, the affected item's fidelity drops to
    handle. From then on pressure, utility and renewal are computed on what the model actually sees.
  - Verify: in a long task with compaction on, the kernel's input token estimate deviates from
    measured usage by <10%; renewal no longer fires early.
  - Files: `context/manager.rs`, `context/pressure.rs`; `node/src/providers/anthropic-compaction.ts`,
    `python/deepstrike/providers/anthropic_compaction.py`,
    `wasm/src/providers/anthropic-compaction.ts`, `rust/src/providers/anthropic.rs`

### Stage 6: converge dynamic orchestration (fixes Q8)

- [x] Task 6.1 (2026-11-01): checking showed the canonical ABI never accepted
  Loop/Classify/Tournament/Reduce (the host rejected them outright, and the three signal fields of a
  child result were always None on the wire), so they were deleted together with Reduce, the
  reducers module and tournament.rs; `gen_eval` drops max_iters.
- ~~Original task 6.1: delete declarative control-flow nodes~~
  - Acceptance: delete `NodeKind::Loop/Classify/Tournament`, `orchestration/tournament.rs` and the
    re-arm, prune and bracket logic in the scheduler; the kernel keeps only `Spawn` and `Reduce`.
    Classify becomes a model-time submission (the comment at `mod.rs:86` already recommends it);
    Loop and Tournament become host programs. The migration notes give rewrite examples for all
    three node kinds, with no same-name template.
  - Files: `orchestration/workflow/mod.rs`, `run.rs`, `scheduler/state_machine/workflow.rs`

- [x] Task 6.2 (2026-11-03): `ctx.blackboard.put/get/delete`; entries are `blackboard:<key>` items on
  the operation (origin `blackboard`, scope `workflow`: every agent spawned afterwards inherits them
  whatever its context inheritance), and phase-scoped entries leave when their phase ends. All four
  SDKs.
- ~~Original task 6.2: write orchestration program shared state as items~~
  - Acceptance: data shared across phases is written as items with
    `blackboard.put(key, value, {scope, lease})` and is visible to sub-agents by scope; it is no
    longer spliced into prompts through closure variables.
  - Files: `node/src/workflow/dynamic.ts`

- [x] Task 6.3 (2026-11-03): Python `run_dynamic_workflow`, WASM `runDynamicWorkflow` and Rust
  `run_dynamic_workflow` with the Node vocabulary; Rust's workflow driving loop became
  `drive_workflow`, and its children now receive `inherited_knowledge`. Replay of saved runs and the
  sandboxed script executors remain Node-only.
- ~~Original task 6.3: bring the SDKs level~~
  - Acceptance: the host program API is ported to Python, Rust and WASM in that order, with the same
    interface as Node. Replay keeps using `DynamicWorkflowReplayFact` as a recorded input (D1);
    byte equality across SDKs is not required.

### Stage 7: the item graph (0.2.81)

**Why.** Items already relate to each other in directed ways, but every relation hides in a
position, a counter or a key convention instead of being stored as a first-class edge:

| Relation (direction) | Represented today by | Gap |
|---|---|---|
| tool result → the call it answers (`answers`) | message positions (`units::unit_boundaries`) | compaction, rendering, renewal and provider summary coverage all rely on position to keep a transaction whole |
| history item → the knowledge it references (`references{grade}`) | counters on the referenced item (`strong_refs` / `lexical_refs`) | which message referenced it is lost, so regret and waste cannot be attributed |
| read → the item read back (`reads_back`: `read_result`, `memory(record_id)`) | a per-origin count | what was read back, and why, is lost |
| provider_summary / compaction summary → the items it covers (`summarizes`) | covered items simply retire | a summary's sources cannot be traced or paged back |
| a child run's inherited copy → the parent item (`inherited_from`); downstream → a dependency output (`depends_on`) | not recorded; the DAG lives only at the workflow level | data flow across runs is cut off from items |

**Model.** Every item (history, knowledge, provider_summary) has an `ItemId`. An edge is
`ItemEdge { from, to, relation, turn }` over a small fixed set of relations; this is not a general
graph store. Constraints:
- edges come only from recorded input (D1), so replay rebuilds the same graph;
- edges are checkpointed and bounded: every edge whose ends are both present, plus the edges into
  the latest `HISTORY_ITEM_DETAIL_CAP` retired items;
- report first, change behavior later: edges take part in decisions only after the lab shows a
  gain (the same lab-first order as 0.2.80).

- [x] Task 7.0 (2026-11-02): `benchmark/context-lifecycle/knowledge-pressure.mjs`: the ledger
  audit's two rules live only in memory, among ten distractors (several also about audits and
  ledgers), with a 4% knowledge budget and a 7000-token window. Trials carry `oracle` counts (the
  scenario knows which items are required): `required_dropped`, `required_unreferenced`,
  `distractor_refs`, `distractor_resident_tokens`, reported and gated by the lab as
  `oracle.<name>`. The 0.2.80 baseline (DeepSeek, 12 tasks, `results/knowledge.jsonl`): 9/12
  correct, regret 0, waste_tokens 24, 0.67 read-backs; the required rules were never evicted and
  always referenced; distractors were credited 33 references on average and 246 tokens of them were
  still resident at the end. The finding: under this budget 0.2.80 does not lose required items;
  the gap is the evidence. Lexical overlap counts distractors as used, so the kernel's own waste
  (24) is far below the real idle residency (246). Task 7.3's target follows: lower
  `oracle.distractor_refs` and `oracle.distractor_resident_tokens` while `required_dropped` stays 0
  and success does not drop.
- ~~Original task 7.0: benchmark first~~
  - Acceptance: a new `knowledge-pressure` scenario: a tight knowledge budget, many irrelevant
    entries and a few the answer needs (memory recall included), with the answer depending on the
    needed ones; a baseline on 0.2.80 (success, regret, waste, read-backs). The ledger and fan-out
    scenarios stay as regressions.
  - Files: `benchmark/context-lifecycle/`

- [x] Task 7.1 (2026-11-02): knowledge items share one id allocator with history (an older
  checkpoint's slots take ids on restore, in slot order); new `context/graph.rs`, with the edge table
  checkpointed (`item_edges`): every edge between resident items is kept, of the rest only the
  latest 128. Recorded edges: `answers`, `references_strong` / `references_lexical`, `reads_back`
  (the assistant item whose `read_result` / `memory(record_id)` call read an item back → that item; a memory search hit is not a read) and
  `summarizes` (provider summary → each covered item). The lifecycle report gains `edges`,
  `edges_omitted` and `waste_items`, and item records carry `id`; the four SDK projections and the
  contract follow. Goldens change only in checkpoint / report fields and their digests. Where it
  differs from the acceptance: the kernel's own compaction creates no summary item, so `summarizes`
  comes from provider summaries only; and a regretted item is put back by a kernel recall, with no
  reading item, so regret is located by the item record's `id` and `paged_in_after_evict` plus the
  edges into it rather than by "a read-back edge".
- ~~Original task 7.1: every item has an id; the edge table only reports~~
  - Acceptance: knowledge items take an `ItemId` from the same allocator (checkpointed; an older
    slot without one is assigned one on restore). The manager holds the edge table and records
    `answers` (tool result → the assistant item holding the call), `references{strong|lexical}`
    (history → knowledge, replacing bare counters), `reads_back` (`read_result` /
    `memory(record_id)` → target) and `summarizes` (summary → covered item). The lifecycle report
    gains capped `edges`; regret names the read-back edge that brought an evicted item back, and
    waste lists knowledge with no incoming edge. Rendering and compaction do not change: goldens
    change only in checkpoint and report fields.
  - Files: `context/item.rs` (`ItemEdge`, `Relation`), new `context/graph.rs`,
    `context/manager.rs`, `context/partitions.rs`, `runtime/kernel/wire/checkpoint.rs`; the
    session-event registry and the four SDK projections

- [x] Task 7.2 (decided against, 2026-11-02): `unit_boundaries` already pairs tool results with
  calls by `call_id`, not by position, and it also keeps the user turn and the trailing answer in
  the unit, which `answers` edges cannot express. Rebuilding units from edge components would add
  no information and could not keep rendering byte-identical. `answers` edges stay in the report,
  for attribution.
- ~~Original task 7.2: transaction units come from `answers` edges~~
  - Acceptance: compaction, the renderer, renewal, provider summary coverage and held history all
    take "units that must not be split" from the connected components of `answers` edges;
    position-based `unit_boundaries` is deleted (or kept only as the fallback for older
    checkpoints).
  - Verification: the random-transcript pairing property tests still pass; existing goldens render
    byte for byte the same.

- [x] Task 7.3 (2026-11-02): evidence counts by its source. Lexical overlap counts as use only in
  the model's own words and the user's; tool output counts only when it names the item (key or
  record id); a memory hit (the record's own text) never counts. A new `expands` edge (memory hit →
  the recall it expands) carries the model's use of a hit's content one hop to the recall. An item
  a resident history item strongly references or read back neither expires nor yields to the
  budget. Results (DeepSeek, 12 tasks, against 0.2.80): the `knowledge-pressure` gate passes,
  with distractor references down from 33 to 9.6, self-reported waste up from 24 to 156 (real idle
  residency 233), no required rule dropped, and 9/12 → 10/12 correct; the fan-out full and handle
  gates pass (12/12); ledger's first 12 scored 10/12 against 11/12 and failed the gate (both misses
  arithmetic, with the rule resident and referenced throughout); with 12 more tasks on each side
  it is 22/24 against 20/24 (one 0.2.80 run ended in an error) and the gate passes. Where it differs from the acceptance: the retention
  score still uses reference counts (which now hold only counted evidence) rather than a separate
  weighted sum of resident in-edges, and the one-hop propagation runs along `expands` edges.
- ~~Original task 7.3: retention and eviction along edges~~
  - Acceptance: an item's retention score adds its incoming edges from items still present (strong
    weighs more than lexical, one hop only, integer weights, deterministic); knowledge-budget
    eviction and recall lease expiry consult incoming edges — an entry a present item strongly
    references is neither evicted nor expired.
  - Verification: `deepstrike-lab gate` on `knowledge-pressure`: regret and waste no higher than
    the 7.0 baseline, success not lower; ledger and fan-out do not regress.

- [x] Task 7.4 (2026-11-02): a spawn's `inherited_knowledge` entries carry
  `inherited_from { operation, item }`, `KnowledgeEntry` accepts the field, and all four SDKs pass it
  to the child. The child records an `inherited_from` external edge, and
  `read_result("dep:<agent>")` records a `depends_on` external edge (the reading assistant item →
  the dependency agent). The lifecycle report gains `external_edges` and `parent_items_used` (the
  parent items the child used). External edges are checkpointed (`item_external_edges`), the latest
  128 kept. Also fixes Python's initial context dropping `pinned`.
- ~~Original task 7.4: edges across runs (report only)~~
  - Acceptance: inherited knowledge carries `inherited_from { operation, item }` and the child run
    records the edge; dependency-output reads are recorded as `depends_on` / `reads_back`. A child
    run's lifecycle report names which parent items it used.
  - Files: `orchestration/workflow`, `runtime/kernel/wire/driver/continuation.rs`, the four SDKs'
    `inherited_knowledge`

### Stage 8: measure first, put the cache into decisions (0.2.82)

**Why.** Stages 0–7 finished the ledger, but measurements on 0.2.79–0.2.81 show that the
bottleneck is no longer missing mechanism: the books are wrong, and decisions ignore cache cost
(`results/ledger.jsonl`, DeepSeek `deepseek-flash`; uncached input = input − 0.9 × cache reads):

| Release | Correct | Input tokens / task | Cache hit rate | Uncached input / task | Kernel-reported `cache_loss_tokens` |
|---------|---------|---------------------|----------------|-----------------------|-------------------------------------|
| 0.2.79 | 11/12 | 52.4k | 63% | 22.7k | 0 |
| 0.2.80 | 20/24 | 57.1k | 53% | 29.6k | 0 |
| 0.2.81 | 22/24 | 54.4k | 56% | 27.1k | 0 |

| # | Problem | Evidence |
|---|---------|----------|
| Q12 | `cache_loss` counts only knowledge promotion (`cache_invalidated_tokens` in `manager.rs`); prefix rewrites from compaction, narration collapse, over-budget drops and renewal are not counted. The kernel reports 0 while the measured hit rate fell from 63% to 56% — the same kind of gap as the under-reported waste found in 7.0 | table above |
| Q13 | Rendering rewrites the middle of the prefix every turn: `collapse_assistant_narration` is on by default and `preserve_recent_turns` = 2, so the narration unit that just left the protected window is replaced by `NARRATION_STUB` on that turn; over budget, the oldest unit is dropped one turn at a time. Both make everything after the break point bill as uncached again, every turn | `project_assistant_narration` and the render loop in `renderer.rs` |
| Q14 | The benchmarks are too small for a gate to tell improvement from noise: 7–8k windows, peak context under 10k, 12 tasks per variant; 9/12 vs 10/12 is noise. Long tasks with 100k+ windows and dozens of turns are not covered, so the data that would show whether `RuleSummarizer` is the bottleneck (see "Out of scope") can never appear | `long-task.mjs` defaults to `--max-tokens 8000`; `knowledge-pressure.mjs` uses a 7000 window |
| Q15 | The gate is driven by token and character counts, not price; a change that saves tokens but loses cache hits passes as an improvement | `Summary` in `crates/deepstrike-lab` |
| Q16 | The success metrics "input −25%, cache_loss −50%" are not met, and the current gate cannot see it | ledger input 52.4k → 54.4k |

**Principles.**
- As in 0.2.80 and 0.2.81, the lab comes before the change it measures: 8.0–8.2 change only
  measurement, not rendering; 8.3 and 8.4 change decisions and must pass the new gate first.
- Cache loss, like waste, is grounded in provider measurements; the kernel's self-reported number
  must agree with them.
- Freeze the graph: this stage adds no relation, lease or origin. Existing edges take part in
  decisions only after the lab shows a gain.

- [x] Task 8.0 (2026-11-11): new `context/cache_ledger.rs`.
  Each request's prefix is fingerprinted segment by segment (tool schemas, system[0], system[1],
  each history turn) and compared with the previous request; the first segment that differs is the
  break. The cause is announced by the rewrite that made it (`snip` / `micro` / `collapse` /
  `auto` / `renewal` / `provider` / `promotion`) or, when none was, read from the render
  (`toolset` / `system` / `narration` / `budget_drop` / `reprojection` / `other`). The lifecycle
  report gains `prefix_breaks` (the latest 128), `prefix_breaks_omitted` and `cache_tail_tokens`;
  `cache_loss_tokens` becomes the sum over all breaks; the ledger is checkpointed. The lab reads
  trials' `cost.requests[]` and reports `observed_cache_loss_tokens`, `cache_loss_error` and
  `cache_loss.<cause>`, with `gate --max-cache-loss-error`. The four SDKs' projections and the
  contract are in sync; the Rust workspace, Node, Python, WASM and four-SDK conformance all pass.
  A scripted long task (`run.mjs`, 25 calls) records two `micro` breaks totalling 17.6k tokens
  where the old field reported 0. Differences from the acceptance:
    - The field is `requests[]`, not `calls[]` (`cost.calls` is already the call count); a break
      is `cause` plus `kept_tokens` / `lost_tokens` (the break position in tokens).
    - Causes add `toolset`, `system`, `reprojection` and `other`: a skill narrowing the toolset
      invalidates the whole cache, a kind of break the plan did not list.
    - New `cache_tail_tokens`: the previous request's state turn is rebuilt every request and was
      never reusable, but a provider's uncached input count includes it; the lab subtracts it from
      the measured value before comparing it with the kernel's.
    - Fingerprints cover only what reaches the provider: a tool result's `durable_content` is left
      out (a restored message carries it and a live one may not, which would otherwise show a
      false break after a restore).
    - A newly found per-turn break source (for 8.3): above the collapse threshold,
      `recompute_handle_residency` collapses one more old tool result every turn.
    - Measured (DeepSeek `deepseek-flash`, 12 tasks each, `0.2.82-8.0`): the kernel's cache loss is
      within 11.6% (ledger) and 7.3% (knowledge-pressure) of the provider-measured loss, so the
      acceptance passes. Ledger loss per task by cause: collapse 7.3k, toolset 5.9k, narration
      5.6k. Success was 10/12 and 8/12; 8.0 does not change rendering, so this is noise, which is
      exactly why 8.1's paired rule is needed.
  - Acceptance: a trial's `cost` gains per-call detail `calls[]` (`input_tokens`,
    `cache_read_tokens`, `cache_write_tokens`, `output_tokens`, and the kernel's prefix break
    point). The lab computes measured cache loss per call as `max(0, reusable prefix − cache_read)`,
    where the reusable prefix is the previous call's input minus this turn's new tail. The kernel's
    lifecycle report gains `prefix_breaks` (turn, action
    `promotion | snip | micro | collapse | auto | narration | budget_drop | renewal | provider`,
    break position, tokens after the break), and `cache_loss_tokens` becomes the sum over all
    breaks instead of knowledge promotion only.
  - Verification: on ledger and knowledge-pressure, the kernel's `cache_loss_tokens` is within 15%
    of measured cache loss (the tolerance covers provider cache granularity, e.g. DeepSeek's
    64-token blocks).
  - Files: `context/manager.rs`, `context/renderer.rs` (render returns the break point),
    `crates/deepstrike-lab`, the metering wrappers in `benchmark/context-lifecycle/*.mjs`; the four
    SDKs' projections and contracts

- [x] Task 8.1 (2026-11-11): the lab gains a price table (`--prices`; the default is relative units:
  uncached input 1, cache read 0.1, cache write 1, output 4; `benchmark/context-lifecycle/prices.json`
  holds only the default, and users fill in real prices) and `weighted_cost` (the report's first
  row); `gate --paired metric:tolerance` pairs runs by (scenario, task) and gives a fixed-seed
  bootstrap 95% interval, failing as "insufficient samples" below `--min-pairs` (30). Trials gain
  `model` and `cache_write_tokens`. Recomputed from existing results: ledger 0.2.79 → 0.2.81 is +11.6%
  on `weighted_cost` where `input_tokens` shows only +3.8%, so the verification passes.
  Differences from the acceptance:
    - A 5-point success tolerance is unusable at 30–40 pairs: with one failure on each side and a
      net difference of 0, the interval is already ±7.5 points. Use `success_rate:0.10`; it
      absorbs noise that cancels out and still fails at three net losses (tested).
  - Acceptance: the lab gains a price table (per provider / model: uncached input, cache read,
    cache write and output prices) and a `weighted_cost` metric, which becomes the primary cost
    gate; `input_tokens` / `sent_chars` become diagnostics. The gate compares the same task across
    two variants as pairs and reports a bootstrap 95% confidence interval on the difference:
    success "does not drop" only if the interval's lower bound is above −5 percentage points; with
    fewer than 30 pairs the gate reports "insufficient samples" instead of passing.
  - Verification: recomputed from the three existing result files, ledger 0.2.79 → 0.2.81 shows a
    regression on `weighted_cost` (uncached input +19% in the table above).
  - Files: `crates/deepstrike-lab/src/lib.rs`, `main.rs`

- [x] Task 8.2 (2026-11-11): `long-horizon.mjs`, an incident investigation. The scoring rules live
  only in `brief.md`, read first (the error code to count, request ids to skip, and a multiplier
  and on-call badge number needed only at the end); then 40 logs of about 3.5k tokens each against a
  100k window. `--dry` prints the task facts without a model, `--rescore` recomputes oracles from
  recorded answers, `--concurrency` runs tasks in parallel. Baseline (DeepSeek, 30 tasks, code before
  8.3): 12/30 correct, 40 calls, 1.7M input tokens and 5.6 compactions per task on average; kernel
  cache loss within 4.6% of measured; by cause micro 164k, collapse 133k, narration 78k, toolset
  9k. Differences from the acceptance:
    - `early_fact_lost` is 0 in all 30 runs: when compaction takes the brief, the model reads it
      again (1.47 times a task). A fact that can be re-read is always recovered, so lengthening the
      task cannot make it non-zero. What runs lose is their own tallies: every wrong answer keeps
      the brief's formula (answer minus badge divides by the multiplier), and 11 of 30 undercount.
      New oracles: `formula_broken` (the answer does not have the brief's form), `count_error`,
      `undercount`.
    - The model often reads several files in one call, so a task takes 15–40 calls rather than 40+
      turns; compaction still happens (2–17 times a task).
    - The renderer's over-budget drop (`budget_drop`) appeared in no measured run in any scenario,
      so 8.3 leaves it alone.
  - Acceptance: new `benchmark/context-lifecycle/long-horizon.mjs`: a window of at least 100k and
    at least 40 turns per task, with a scripted tool environment that returns large outputs (a
    multi-account ledger or a repository investigation). The answer depends on facts that appear in
    the first 10 turns and are never repeated, so they must pass through compaction or renewal.
    Oracles: `early_fact_lost` (a required fact is neither visible nor read back when answering),
    `compactions`, `renewals`. Baseline on 0.2.81 with at least 30 tasks per variant. The three
    existing scenarios stay as regressions.
  - Verification: the baseline triggers at least one auto compaction or renewal;
    `early_fact_lost` has a non-zero baseline — otherwise lengthen the task until it does.
  - Files: `benchmark/context-lifecycle/`

- [ ] Task 8.3 (2026-11-11; none of three variants passed the gate; not merged, candidate kept on
  branch `exp/8.3c`). Measured against the same baselines (`0.2.82-8.0` / `0.2.82-baseline`, 30
  tasks per scenario):
    - The plan's formula was wrong: it multiplied both sides by (1 − discount), which cancels. The
      right account: the saving is freed tokens × amortized calls × the read discount (freed tokens
      were only ever billed at the cached price), and the cost is tokens after the break ×
      (1 − discount). At the defaults a mid-history rewrite almost never pays.
    - 8.3 (light-tier cost gate + narration collapse only at boundaries + batched tool-result
      collapse + sticky `read_result`): ledger passes (cost −39%, success 23/30 → 27/30);
      knowledge-pressure lacks power (favorable means, intervals across 0); long-horizon cost
      +20%, and 6 of 30 runs hit the turn limit. Deferring the light tiers lets the run climb to
      the hard tier, which archives whole turns instead of excerpting old tool results in place, so
      the model loses its own tallies and re-reads files and the brief (1.47 → 3.23 times). The
      cost account priced the cache and not the information.
    - 8.3b (gate removed): long-horizon cost −0.7%, success 8/30 (not significant), still 5 of 30
      at the turn limit. Toolset loss rose 8.8k → 22.9k: batching delayed the first eviction, so
      `read_result` first appeared when the prefix was largest.
    - The turn-limited runs end writing DeepSeek's tool-call markup as plain text; keeping old
      narration verbatim until a boundary leaves such leaked text in view for many turns. 11 of 60
      runs across the two variants hit the limit, against 1 of 30 in the baseline.
    - 8.3c (narration collapses per turn again; `read_result` offered from the first tool result on
      and kept): the provider account ran out of balance during the runs, and 16 of 30 ledger and
      10 of 30 long-horizon runs ended in `402 Insufficient Balance`. The data is invalid and moved
      to `results/partial-8.3c.jsonl`.
    - Outcome: this release keeps 8.0's rendering by default; the code is on `exp/8.3c`
      (`c8240078`). The next measurement starts from 8.3c, 30 tasks each on long-horizon and ledger.
    - Ruled out: a cost gate on the light tiers (it trades information for cache), and narration
      collapse only at boundaries (loop risk).
  - Acceptance: compaction and render-time projections do the arithmetic before acting: an action
    that creates a prefix break runs only if "tokens freed × amortized calls × (1 − cache read
    discount)" exceeds "tokens after the break × (1 − cache read discount)". `ContextPolicy` gains
    `cache_read_discount_ppm` (default 100_000) and `amortize_calls` (default 8); the hard window
    limit takes precedence, and over the limit compaction runs as before. Concretely:
    - narration collapse no longer happens unit by unit on every render; it runs in batches at a
      boundary or during compaction (fixes Q13);
    - dropping the oldest units over budget drops down to `target_after_compress` in one go
      instead of one unit per turn;
    - one compaction produces one break where possible: all rewrites start from the same earliest
      position instead of breaking at several positions.
  - Verification: `deepstrike-lab gate` on ledger, knowledge-pressure and long-horizon shows lower
    `weighted_cost` and no drop in success (paired rule from 8.1); fan-out does not regress.
    Goldens change; re-bless them and describe the change in MIGRATION.
  - Files: `context/compression.rs`, `context/renderer.rs`, `context/pressure.rs`,
    `context/policy.rs`, `context/config.rs`

- [x] Task 8.4 (2026-11-11; not done, precondition not met): long-horizon, four variants, 120 runs
  (8.3c only partly valid):
    - `early_fact_lost` fired 5 times in all: once in 8.3b and 4 times in 8.3c (whose data is
      invalid); it is 0 in the baseline and in 8.3. When compaction takes the brief, the model reads
      it again (1.2–3.2 times a task).
    - Wrong answers are mostly miscounts, and they do not follow compaction: correct baseline runs
      averaged 6.1 compactions, undercounting ones 3.7; undercounts occur with 2 compactions and 4
      archived items.
    - `RuleSummarizer` is keyword-slotted and does drop running tallies such as "7 so far", but
      the data says that is not what fails runs, so a model summary would not buy accuracy.
    - The precondition does not hold, so the task is not done. If a later scenario shows failures
      concentrated in summarized ranges, implement it per the original acceptance.
  - Precondition: in the long-horizon baseline, `early_fact_lost` happens mostly inside ranges
    covered by `RuleSummarizer` summaries. Otherwise skip this task and record the data.
  - Acceptance: on auto compaction the kernel emits a `summarize` effect (the item range to
    archive, with a token cap); the host calls a model and the summary comes back to the kernel as
    a recorded fact (D1, so replay yields the same graph), written as a `summary` item with
    `summarizes` edges to the covered items. If the effect fails or times out, fall back to
    `RuleSummarizer`. `summarizes` edges then also come from the kernel's own compaction, and the
    covered originals can be read back along them.
  - Verification: on long-horizon, `early_fact_lost` falls, success does not drop, and
    `weighted_cost` (including the summary calls) does not rise.
  - Files: `context/summarizer.rs`, `context/compression.rs`, `runtime/kernel/wire/effect.rs`; effect
    execution in the four SDKs

Deferred items stay deferred: the model `RelevanceJudge` and Anthropic reporting compaction facts
are scheduled once long-horizon can measure them.

## Handoff (2026-11-02)

Branch `feat/context-lifecycle-stage0`. Everything below is committed (`fda3aeb6` onward):

- `orchestration/workflow/mod.rs`: new `DependencyContext::readable`; only a dependency output the
  brief clipped is sent with the spawn (`dependency_outputs`), and an output shown whole is no
  longer offered through `read_result`. Test `only_a_clipped_output_is_readable`; golden
  `golden_lifecycle_workflow_root.json` re-blessed.
- `context/renderer.rs`: the turn-1 `→ focus:` footer repeats only the first paragraph of the goal
  (at most 240 characters). The footer used to copy the whole goal, and the goal had the dependency
  brief appended, so the brief was sent twice per call (24k → 49k). Test
  `the_focus_footer_repeats_only_the_goal_head`.
- `benchmark/context-lifecycle/live.mjs`: new file, see task 4.3 step two; a provider error is
  written to the result's `error` instead of showing up as an empty answer.
- Verified: the four SDKs, contracts, parity and conformance all pass; CHANGELOG / MIGRATION are
  updated.

Known issues:

- ~~DeepSeek replay~~ (fixed): the model did give reasoning, and streaming accumulation did not lose
  it. At render time the kernel replaces old assistant narration that has left the protected window
  (≥40 characters with tool calls) with `NARRATION_STUB`, while Node / Python stored and looked up
  reasoning replay by `content + tool_calls`; once the content changed the lookup missed and
  `requireReasoningReplay` threw. Handle mode makes more tool calls, so old narration turns leave
  the window sooner and the bug showed more often. Fix: OpenAI-chat reasoning replay keys a turn
  with tool calls by its tool calls only (Node `reasoningReplayKey`, Python
  `reasoning_replay_key`); Anthropic native blocks are still keyed by content. The WASM and Rust
  OpenAI providers do not replay `reasoning_content` and are unaffected. Tests: Node
  `provider-narration-replay.test.ts`, Python
  `test_deepseek_replays_reasoning_after_the_kernel_rewrites_the_turn_text`.
- The `benchmark-0.2.74` version guard (0.2.74 vs 0.2.77) has long been stale; not handled.

Next: 0.2.78–0.2.81 are released and stages 0–7 are done. The next release, 0.2.82, is stage 8
(measure first, put the cache into decisions); see "Release plan" for the order. Deferred: the
model `RelevanceJudge` (another source of in-edge weight) and Anthropic reporting compaction facts,
scheduled once the long-horizon benchmark can measure them.

Setting up on another machine:

- The baseline worktree lives in a local temporary directory; rebuild it with
  `git worktree add ../deepstrike-base 5b5308d3`, then build `node/dist` in it
  (`npx napi build --platform --release` under `crates/deepstrike-node`, `npm run build` under
  `node`).
- This branch also needs the napi addon and `node/dist` rebuilt first, or the Node tests and the
  benchmark run an old kernel.
- `.env` is not in the repository; it needs `DEEPSEEK_API_KEY` and `DEEPSEEK_MODEL` (this run used
  `deepseek-flash`).
- WASM tests need the rustup toolchain on PATH; Python tests use a separate venv (the repository's
  own venv is unusable).
- When goldens change, regenerate with
  `BLESS_KERNEL_RECORD_FIXTURES=1 cargo test -p deepstrike-core --lib <test name>`.

## Release plan (2026-11-02)

No jump to 0.3.0. One branch and one PR per release, released after merge; every release with
breaking changes gets its own `MIGRATION-<previous>-to-<this>.md`.

| Release | Content | Status |
|---------|---------|--------|
| 0.2.78 | Merged into main (PR #4), from branch `feat/context-lifecycle-stage0`: stages 0, 1.2, 2.1, 2.2, 3.1, 3.2, 4.1, 6.1; the item identity parts of 1.1a and 1.1b; replay fixes; default edge mode `handle`; Rust workflow orchestration | Released |
| 0.2.79 | Finish 1.1 completely: `ContextPartitions` as a render view over the item table, with the compression pipeline operating on items; the `until(event)` lease; scope / fidelity / authority with behavior (scope decides renewal and child-run inheritance, fidelity decides the rendered form, authority decides who may change an item); handles folded into the item table; per-item history detail (capped) | Released (PR #5) |
| 0.2.80 | A larger release that includes the former 0.2.81: 4.3 lab tool; 1.1c stable item ids in the message ABI and 5.1 absorbing provider rewrites; 4.2 graded reference evidence; 6.2 blackboard; 6.3 Python, WASM and Rust host program APIs. `MIGRATION-0.2.79-to-0.2.80.md` | Released (PR #6; deferred: the model `RelevanceJudge`, Anthropic reporting compaction facts) |
| 0.2.81 | Stage 7: the item graph. 7.0 the `knowledge-pressure` benchmark and a 0.2.80 baseline; 7.1 ids for every item, an edge table that only reports; 7.2 transaction units from `answers` edges; 7.3 retention and eviction along edges; 7.4 edges across runs (report only). `MIGRATION-0.2.80-to-0.2.81.md` | Released (2026-11-03; 7.2 decided against) |
| 0.2.82 | Stage 8: measure first, put the cache into decisions. 8.0 real cache loss with `prefix_breaks` attribution; 8.1 a price-weighted gate with paired statistics; 8.2 the `long-horizon` benchmark with a 0.2.81 baseline; 8.3 cache-aware compaction; 8.4 model summaries as an effect (depends on 8.2 data). `MIGRATION-0.2.81-to-0.2.82.md` | 8.0–8.2 done; 8.3 failed the gate and is not merged (`exp/8.3c` awaits measurement); 8.4 not done, precondition not met |

0.2.82 internal order: 8.0 → 8.1 → 8.2 → 8.3 → 8.4. 8.0–8.2 change only measurement, not
rendering, and produce a new baseline on 0.2.81 first; 8.3 is the only item in this release that
changes rendering by default and must pass the new gate first; 8.4 is done only if 8.2's data meets
its precondition.

0.2.81 internal order: 7.0 → 7.1 → 7.2 → 7.3, with 7.4 interleavable with 7.2 / 7.3. 7.1 and 7.2
leave rendering unchanged; only 7.3 changes decisions and must pass the lab gate first. If the model
`RelevanceJudge` is built, it comes after 7.3 as another source of incoming-edge weight, compared on
the same scenario.

Order inside 0.2.80 (the lab comes before the changes it measures; otherwise a metric change cannot be
attributed):

1. 4.3 lab tool (`crates/deepstrike-lab`), with a baseline run on the 0.2.79 code first: regret,
   waste, cache_loss, success rate, cost.
2. 1.1c + 5.1: stable item ids in the message ABI, provider replay keyed by id (replacing content
   keys, Anthropic native blocks included); provider compaction and `clear_tool_uses` absorbed as
   facts (the two share one ABI extension). Use the lab to confirm the kernel's token estimate is
   within 10% of measured usage.
3. 4.2 graded reference evidence and `RelevanceJudge`, compared with the lexical-only version in the
   lab: regret and waste no higher, success rate no lower.
4. 6.2 blackboard, 6.3 Python / WASM host program APIs: independent of the first three, can be
   interleaved.

The PR is large, so the steps above land as staged PRs into one release branch, with the version
bumped once at the end.

Remaining 0.2.78 steps:

1. ~~Rerun the three variants with 24 questions each~~ (done; results under the 4.3 step-two
   rerun). ~~Default edge mode~~: now `handle`, see MIGRATION.
   Side fix: a tool-call turn on which DeepSeek gave no reasoning replays an empty
   `reasoning_content`.
2. ~~The Rust part of task 6.3~~ (done, `34850587`): workflow nodes run as child runs on the parent
   runner's provider and plane, with their dependency outputs; model-submitted `start_workflow` /
   `submit_workflow_nodes` arguments are lowered to the canonical DAG. Not done: the signal-driven
   preemption monitor (Node's `monitorWorkflowPreemption`), writing node completion to the session
   log for resume, and exporting the Rust `start_workflow` tool schema.
3. ~~Sync the deletion list, bump the version to 0.2.78, open the PR~~ (done, PR #4).
4. ~~Fold the item identity parts of 1.1a and 1.1b into 0.2.78~~ (done). PR #4's docs check failed
   on the missing English plan and `memory-candidate-abi`; both `docs/en/` versions are added.

Loose ends (2.1 recall on plan change, 3.2 token cost of Full, 3.1 `fields` slices, the
`benchmark-0.2.74` version guard) are not scheduled separately; they are done in whichever release
touches the related code.

## Deletion list

| Deleted | Stage |
|---------|-------|
| ~~`context/execution.rs`, `evolution.rs`, the four SDKs' `shared_evidence.*` and `evolution.*`~~ (deleted) | 0.1 |
| ~~the `context_prepared` event, `prepare_context_dispatch`, digest comparison in conformance, `EvaluationContextBinding`~~ (deleted) | 0.1 |
| ~~`verifiable.rs`, chain_validator, the CLI~~ (deleted; C1/C3 covered by the restore self-check) | 0.2 |
| ~~`KnowledgeEntry`~~ (deleted); positional partition types such as `Partition` (replaced by the item table in 0.2.79) | 1 |
| ~~the four `prefetchMemory` and the projection logic of the three `recall-projection.*`~~ (deleted; the files keep only the `memoryFetchRecall` point lookup) | 2 |
| ~~the three `workflow-control-flow.*` (including `summarizeDependency`) and the edge `summary` mode~~ (deleted; the `reference` mode stays) | 2, 3 |
| ~~`NodeKind::Loop/Classify/Tournament`, `orchestration/tournament.rs`~~ (deleted, together with Reduce) | 6 |
| ~~comments and docs claiming "knowledge appends are cheap"~~ (fixed: `partitions.rs`, `docs/concepts/prompt-cache-design.md`) | 1 |

## Dependencies and order

```text
stage0 ──► stage1 ──► stage2 ──► stage3 ──► stage6
              ├──► stage5
              └──► stage4.2/4.3 ──► stage7 (item graph) ──► stage8 (8.0/8.1/8.2 ──► 8.3 ──► 8.4)
stage4.1 runs in parallel with stage1 and produces the baseline first
```

Stage 0 and stage 6.1 are pure deletions and could each ship on their own; the other stages follow
their dependencies.

## Out of scope

- The memory write side (candidate ABI, `MemoryWriter`) is frozen; no new write mechanism.
- ~~The compression pipeline's internal `RuleSummarizer` is not replaced by model summaries in
  this plan; revisit once 4.1 data shows it is the bottleneck.~~ Moved into task 8.4: the existing
  benchmarks never reach the scale where summaries matter; 8.2's long-horizon provides the data.
- Stage 8 adds no new relation, lease or origin kind.

## Risks

| Risk | Mitigation |
|------|------------|
| Without the evidence layer, incident investigation loses the `context_prepared` record | journal + checkpoint are enough to reconstruct every turn's context decisions; rebuild the rendered result with `inspect`/`replay` when needed |
| Model relevance judgments add cost and latency | batch scoring, a small model, fire only above a pressure threshold, lexical fallback |
| Durable items wait for a boundary to be promoted and may arrive too late | an `urgent` switch with its cost recorded; watch regret |
| In handle mode sub-agents do not page in on their own | better previews and an explicit hint in the brief; evaluate the default edge mode by regret and success rate |
| Deleting NodeKind makes existing declarative WorkflowSpecs unrunnable | accept the break; the migration notes give rewrite examples, and loading an old spec errors and points at them |
| The item graph makes checkpoints and reports larger | edges are bounded (both ends present, plus the latest retired batch), and report edges are capped |
| Judging every history message against every knowledge item to build edges adds cost | reuse the 4.2 distinctive-term sets, cache them per item; the lab records kernel time per turn |
| Propagating along edges amplifies one misjudged reference | one hop only, strong weighs more than lexical; 7.3 must pass the lab gate |
| Providers differ in cache granularity and rules, so measured cache loss is noisy | 8.0 uses a 15% tolerance; attribution only compares variants on the same provider |
| Deferring compaction keeps the context longer and may hurt attention quality and success | the hard window limit takes precedence; 8.3's gate checks success and `weighted_cost` together |
| Each long-horizon task is expensive, so samples may be too few | a scripted tool environment with a real model only for answering; 30 tasks per variant, compared as pairs |
| The summary effect adds a model call and latency | fires only on auto compaction; summary calls count toward `weighted_cost`; falls back to `RuleSummarizer` on failure |

## Success metrics

- Long-task benchmark: at no loss of success rate, average input tokens −25%, cache_loss −50%,
  regret and waste with a baseline and falling release by release
- The four runners −30% lines in total; core shrinks by about 7k lines net after deleting the
  evidence layer (`chain_validator` 3.8k, `evolution.rs` 1.3k, `execution.rs` 1.2k,
  `verifiable.rs` 0.7k, minus the kept C1/C3)
- Changing one lifecycle policy touches one place in the kernel (a PR check item)
- fan-out→synthesize: downstream initial tokens independent of fan-out width
- With server compaction on, the kernel's estimate deviates from measured tokens by <10%
- Stage 8 (0.2.82): the kernel's self-reported cache_loss within 15% of measured; `weighted_cost`
  on ledger and long-horizon below the 0.2.81 baseline with success not lower (paired 95%
  confidence interval)
- Item graph (0.2.81): on `knowledge-pressure`, regret and waste below the 0.2.80 baseline with
  success not lower; every regret and every wasted item points at a specific edge in the report
