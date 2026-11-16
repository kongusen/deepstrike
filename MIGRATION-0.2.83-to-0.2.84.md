# Migration from 0.2.83 to the next release

The version number is provisional. This release is phase 0 of the
[kernel-distillation plan](./docs/plans/kernel-distillation.md) plus the first results of its
stage 4 (memory under drift).

## Stored state

Checkpoints and session logs written by 0.2.83 load unchanged. A `summary_upgraded` event in an
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

## Changed defaults

- `ContextConfig::default().notes_budget_tokens` is 600, the wire default. Code that builds a
  `ContextManager` directly gets the `note` tool; set the field to `0` to turn it off.
