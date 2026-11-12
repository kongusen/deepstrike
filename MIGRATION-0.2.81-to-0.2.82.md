# Migration from 0.2.81 to 0.2.82

0.2.82 is stage 8 of the [context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md):
measure first, then put the cache into decisions. This note grows with each task of the stage.

## Stored state

0.2.82 restores checkpoints written by 0.2.81: a checkpoint without `cache_ledger` restores with an
empty ledger, so the first request after the restore is not compared with anything. Checkpoints
written by 0.2.82 carry the new optional field `cache_ledger`; 0.2.81 cannot read them.

## Behavior changes

- `context_lifecycle_reported.cache_loss_tokens` changes meaning (plan task 8.0): it is now the sum
  of every prefix break between consecutive provider requests of the run, whatever caused it. In
  0.2.81 it counted only the history behind a mid-run knowledge promotion, so it was 0 in most runs.
  Do not compare the field across the two versions; compare `deepstrike-lab`'s
  `observed_cache_loss_tokens`, which comes from provider usage.
- `context_lifecycle_reported` gains optional `prefix_breaks`, `prefix_breaks_omitted` and
  `cache_tail_tokens`. A consumer that validates the event strictly should accept them. The break
  causes are `toolset`, `system`, `promotion`, `collapse`, `reprojection`, `narration`,
  `budget_drop`, `snip`, `micro`, `auto`, `renewal`, `provider` and `other`.
- DeepSeek on the OpenAI-chat protocol (plan task 9.0): tool-call markup the model writes as text
  no longer appears in text deltas or the assistant message; it becomes tool calls with ids
  `call_text_…`, or, when it does not parse, a retryable `protocol` provider error with code
  `textual_tool_call`. A host that scraped tool calls out of DeepSeek's text should stop doing so.
- Rust `OpenAIProvider` now emits every tool call of a turn, in index order, and the usage event
  after them; WASM `OpenAIProvider` no longer drops tool calls when the stream ends with `[DONE]`.
- Notes (plan task 9.1) are off unless a host sets `notesBudgetTokens` / `notes_budget_tokens`, so
  nothing changes by default. With notes on, the toolset gains the kernel tool `note` (never
  executed by the host; a host that filters tools by name should let it through like `update_plan`),
  the state turn gains a `notes:` block, item reports may carry origin `note` and relations
  `derived_from`, `links_to` and `supersedes`, and `context_lifecycle_reported` may carry
  `notes_written` and `notes_active`. Checkpoints carry `task_state.notes` only when notes exist;
  0.2.81 cannot restore such a checkpoint.
- Compaction reading the notes (plan task 9.2) is off unless `compactionReadsNotes` /
  `pageOutNotice` (Python and Rust: `compaction_reads_notes` / `page_out_notice`) are set. With
  `compactionReadsNotes`, `compressed.summary` is an archive locator (`archived N turn(s), …`), not
  the rule digest; with `pageOutNotice`, a collapse can run one turn later than the pressure
  threshold alone would put it.
- Rendering, compaction and provider requests are unchanged. Cache-aware compaction (plan task
  8.3) was measured and is not part of this release; see the plan.

## Benchmarks

Trials gain `cost.requests[]` (each request's `input_tokens`, `cache_read_tokens`,
`cache_write_tokens`, `output_tokens`, and `run` when a trial has several runs). Trials recorded by
0.2.81 have no request detail: `deepstrike-lab` shows no `observed_cache_loss_tokens` or
`cache_loss_error` for them, and `gate --max-cache-loss-error` fails a candidate without it.
Trials also gain `model` and `cost.cache_write_tokens`, which `weighted_cost` prices; a trial
without them is priced at the default prices with no cache writes. `gate --paired` needs at least
`--min-pairs` (30) runs of the same tasks under both variants.

`deepstrike-lab gate` runs the paired checks `success_rate:0.05,weighted_cost:0` when `--paired`
is not given, so a gate that used to pass on summary checks alone can now fail, or fail with
"insufficient samples" below 30 pairs. Pass `--paired none` for the old behavior.
