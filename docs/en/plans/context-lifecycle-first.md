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
  - Not done (a later 0.2.79 item): per-item history detail (history is reported as a per-origin
    tally; listing every item needs a cap or the report of a long run grows too large).
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

- [ ] Task 4.2: reference evidence and relevance judgment
  - Acceptance: reference evidence has three strengths: the model explicitly calls a handle or
    `memory(record_id)` > the item's key appears in tool call arguments > lexical overlap.
    `RelevanceJudge` has a lexical (fallback) and a model implementation; the model one is an
    effect that scores in batches and only fires above a pressure threshold, and its result is
    written as a fact into `value.judged_relevance` (D1). Utility scoring and knowledge eviction
    both read that value.
  - Verify: on a fixed long-task benchmark, regret and waste are no higher than the lexical-only
    version and success rate does not drop.
  - Files: `context/utility.rs`, `context/partitions.rs`, new `context/relevance.rs`

- [ ] Task 4.3: offline tuning
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

- [ ] Task 5.1: absorb provider rewrites of the context
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

- [ ] Task 6.2: write orchestration program shared state as items
  - Acceptance: data shared across phases is written as items with
    `blackboard.put(key, value, {scope, lease})` and is visible to sub-agents by scope; it is no
    longer spliced into prompts through closure variables.
  - Files: `node/src/workflow/dynamic.ts`

- [ ] Task 6.3: bring the SDKs level
  - Acceptance: the host program API is ported to Python, Rust and WASM in that order, with the same
    interface as Node. Replay keeps using `DynamicWorkflowReplayFact` as a recorded input (D1);
    byte equality across SDKs is not required.

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

Next: see "Release plan" below. 0.2.78 merges once CI passes; 0.2.79 follows.

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
| 0.2.78 | Current branch `feat/context-lifecycle-stage0`: stages 0, 1.2, 2.1, 2.2, 3.1, 3.2, 4.1, 6.1; the item identity parts of 1.1a and 1.1b; replay fixes; default edge mode `handle`; Rust workflow orchestration | Awaiting merge |
| 0.2.79 | Finish 1.1 completely: ~~`ContextPartitions` as a render view over the item table, with the compression pipeline operating on items~~ (done); the `until(event)` lease; scope / fidelity / authority with behavior (scope decides renewal and child-run inheritance, fidelity decides the rendered form, authority decides who may change an item); handles folded into the item table; per-item history detail (capped) | In progress, branch `feat/context-items-0.2.79` |
| 0.2.80 | A larger release that includes the former 0.2.81: 4.3 lab tool; 1.1c stable item ids in the message ABI and 5.1 absorbing provider rewrites; 4.2 graded reference evidence and `RelevanceJudge`; 6.2 blackboard; 6.3 Python and WASM host program APIs. One `MIGRATION-0.2.79-to-0.2.80.md` | Not started |

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
              └──► stage4.2/4.3
stage4.1 runs in parallel with stage1 and produces the baseline first
```

Stage 0 and stage 6.1 are pure deletions and could each ship on their own; the other stages follow
their dependencies.

## Out of scope

- The memory write side (candidate ABI, `MemoryWriter`) is frozen; no new write mechanism.
- The compression pipeline's internal `RuleSummarizer` is not replaced by model summaries in this
  plan; revisit once 4.1 data shows it is the bottleneck.

## Risks

| Risk | Mitigation |
|------|------------|
| Without the evidence layer, incident investigation loses the `context_prepared` record | journal + checkpoint are enough to reconstruct every turn's context decisions; rebuild the rendered result with `inspect`/`replay` when needed |
| Model relevance judgments add cost and latency | batch scoring, a small model, fire only above a pressure threshold, lexical fallback |
| Durable items wait for a boundary to be promoted and may arrive too late | an `urgent` switch with its cost recorded; watch regret |
| In handle mode sub-agents do not page in on their own | better previews and an explicit hint in the brief; evaluate the default edge mode by regret and success rate |
| Deleting NodeKind makes existing declarative WorkflowSpecs unrunnable | accept the break; the migration notes give rewrite examples, and loading an old spec errors and points at them |

## Success metrics

- Long-task benchmark: at no loss of success rate, average input tokens −25%, cache_loss −50%,
  regret and waste with a baseline and falling release by release
- The four runners −30% lines in total; core shrinks by about 7k lines net after deleting the
  evidence layer (`chain_validator` 3.8k, `evolution.rs` 1.3k, `execution.rs` 1.2k,
  `verifiable.rs` 0.7k, minus the kept C1/C3)
- Changing one lifecycle policy touches one place in the kernel (a PR check item)
- fan-out→synthesize: downstream initial tokens independent of fan-out width
- With server compaction on, the kernel's estimate deviates from measured tokens by <10%
