# Migration from 0.2.83 to the next release

The version number is provisional. This release is phase 0 of the
[kernel-distillation plan](./docs/plans/kernel-distillation.md) plus the first results of its
stage 4 (memory under drift).

## Stored state

**Operations still in flight under 0.2.83 cannot be resumed by this version.** Their kernel
checkpoints and journal records carry configuration and task-state fields this release removes
(see Removed), and the kernel decodes both strictly and re-hashes what it decodes, so a restore
fails (`record_corrupted`, or a checkpoint that does not decode). Let in-flight operations finish,
or abandon them, before upgrading.

Session logs (the event stream) written by 0.2.83 still load. A `summary_upgraded` event in an
older session log is ignored; replay uses the `compressed` event's rule-based summary.

## Behavior changes

- **Memory headers carry the record's name** (plan task 4.4). What the model sees changes in three
  places:
  - a recalled index line is `[MEMORY record_id=<id> name=<name> kind=<kind> trust=<trust>] <summary>`
    (was `[MEMORY record_id=<id> kind=<kind> trust=<trust>] <summary>`);
  - a search hit and a record read with `memory(record_id)` are
    `[MEMORY record_ref=<id> name=<name> kind=<kind>] <content>`
    (was `[MEMORY record_ref=<id> kind=<kind>] <content>`).

  A record without a name renders as before. A host or test that matches these lines exactly must
  allow for the new field; one that reads the id up to the first space keeps working. The `memory`
  tool's description names the new field.

  Why: `propose_memory` asks the model to reuse an existing name to update a record, and the name
  was never shown. Measured on `multi-session --drift 4` (DeepSeek, 80 paired sessions): proposals
  that updated an existing record 19% → 82%, records per sequence 4.9 → 1.3, correct 30 → 37,
  price-weighted cost per session −11%. Without drift (48 paired sessions): correct 33 → 33,
  cost −10%.
- A model-proposed memory record's `provenance.evidence_refs` gains kernel-written entries after
  the model's own: `call:<id>` for each tool result the references resolve to, and
  `proposal:<call id>` for the proposing call. Node and WASM records now carry
  `provenance.session_id`. Code that treats `evidence_refs` as the model's text only should skip
  entries with these prefixes.
- A model `update_plan` call that sets `directives`, `preserved_refs` or `scratchpad` is refused
  with an error result instead of being applied. Hosts that relied on the model writing these
  should write them with a host task update instead.
- A `propose_memory` outcome (`[MEMORY PROPOSAL name=…] …`) is written into history after the
  tool results of the turn that proposed it, no longer between them. Runs that ended with
  "assistant tool_calls with no tool result" on OpenAI-compatible providers no longer do.

- **A call to an unexposed kernel tool no longer ends the run.** The model gets an error result
  (`Tool '<name>' is not available on this turn. Choose from the tools listed in your tools
  schema.`), nothing is executed, and the turn continues; repeats count toward the repeat fuse.
  Hosts that treated an `invalid_authority` fault from such a call as a terminal outcome will no
  longer see it.
- **`read_result` stays offered once offered.** Before, it left the toolset when the last evicted
  result came back into context. A test that asserts the exact toolset after a page-in, a
  compaction or a renewal sees `read_result` where it did not before. Checkpoints gain an optional
  `read_result_offered`; a checkpoint without it restores as `false`.

## Removed

- **Context policy options** (stage 2 of the kernel-distillation plan; see the CHANGELOG for the
  measurement): `collapseOldAssistantNarration` / `collapse_old_assistant_narration`,
  `idleMicroCompactMinutes` / `idle_micro_compact_minutes`, and the `snip` entry of
  `pressureThresholds` / `pressure_thresholds` / `pressure_thresholds_ppm`. The kernel's wire
  config rejects them as unknown fields, so remove them from host code. The remaining thresholds
  must satisfy `micro < collapse < auto < renewal`, and `targetAfterCompress` must be below
  `micro` (it was below `snip`). `micro` defaults to 0.70.
- **What the model sees** changes in long runs: old assistant text is no longer replaced by a stub,
  the state turn has no footer, and a compaction's record in `compression_history` is one line
  (`[context_collapse] archived 3 messages (~569 tokens); tools used: read_file`) instead of a
  slotted rule summary. A host or test that looked for `[Compressed: …]` markers or `→ focus:`
  should look for these lines instead.
- Compaction events no longer report `snip_compact`; cache breaks are no longer attributed to
  `narration` or `snip`. SDKs still accept those strings when reading older session logs.
- Checkpoints no longer carry `recent_actions`, `last_activity_ms` or `last_compact_ms`.
- Rust: `context::utility` and `context::summarizer` are gone (`context::archive::plan_archive`
  replaces the planner); `EvictionOp` is `Micro | Collapse { target_tokens } | AutoCompact`;
  `plan_eviction` takes no idle flag; `PressureAction::SnipCompact`,
  `KernelPressureAction::SnipCompact`, `ContextConfig.snip_threshold`,
  `ContextConfig.micro_compact_idle_minutes`, `ContextConfig.collapse_assistant_narration`,
  `ContextManager::{record_activity, should_time_decay_compact, recompute_handle_residency,
  note_tool_actions, compress_with_time}` and `TaskState.recent_actions` are removed;
  `compress_with_target` takes no timestamp.

- Node `RunnerOptions.asyncSummarizer` and the `AsyncSummarizer` type. There is no replacement:
  the kernel does not call a model to manage context (DEC-13).
- The `summary_upgraded` session event, from the contract and the Node, WASM and Python types.
- `ContextConfig.autocompact_buffer`, `ContextConfig::autocompact_threshold()` and
  `ContextConfig.verbose_control_notes`.
- Rust: the fields of `EvictionOp::Snip` and `EvictionOp::AutoCompact`, the `preserve_turns`
  argument of `plan_eviction`, and `ContextManager::plan_compaction_params` (use
  `compaction_target_tokens`).
- Rust: `CollapseCompactor` and `AutoCompactor` are one `ArchiveCompactor`.

- Rust: `ContextPartitions.signals` is `Vec<SignalItem>` (`text`, `delivered`) instead of
  `Vec<String>`; read `signal.text`. `Lease::SingleUse` is renamed `Lease::UntilDelivered`.
- Checkpoints gain an optional `signals_delivered` (the signals the in-flight provider request
  carries). A checkpoint without it restores as before.

- **`update_plan` takes `plan` and `current_step` only** (DEC-13 audit, M9). `progress`,
  `blocked_on` and the scratchpad are gone from the task state, from the `[TASK STATE]` block and
  from `TaskUpdate` in every SDK (Node / WASM `progress`, `scratchpad`, `blockedOn`; Python and
  Rust `progress`, `scratchpad`, `blocked_on`). A model call that still passes them is answered
  with a `malformed arguments` error result and the run goes on; a host `update_task` command that
  passes them is rejected as an unknown field. The `update_plan` description reads "Update your
  task plan. …". Measured as part of the stage 2 `minimal` variant: no scenario was significantly
  better with them.
- **`EntropyWatch.notify_model`** (Node / WASM `entropyWatch.notifyModel`, Python
  `entropy_watch["notify_model"]`, the wire `entropy_watch.notify_model`). The entropy alert goes to
  the host only, as the `entropy_alert` event; a host supervisor that wants the model told injects
  its own note. The wire config rejects the field as unknown.

- Rust: `context::policy` (`ContextPolicy`, `PressureThresholdsPpm`, `PPM_SCALE`) is removed; the
  wire `ContextPolicy` is the only one. `ContextManager::apply_context_policy(&ContextPolicy)` is
  `apply_config(ContextConfig)`, and `ContextConfig.preserve_recent_turns` is merged into
  `preserve_recent_units`, which the wire's `preserve_recent_turns` now sets for render as well as
  compaction.

## Changed defaults

- `ContextConfig::default().notes_budget_tokens` is 600, the wire default. Code that builds a
  `ContextManager` directly gets the `note` tool; set the field to `0` to turn it off.
