# Migration from 0.2.81 to 0.2.82

0.2.82 is stages 8 and 9 of the [context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md):
measurement first, then context the model leads — notes on by default — under the principles of
[DEC-13](./docs/en/decisions/013-model-led-context.md). Mechanisms that were measured and failed
are not part of the release.

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
- **Notes (plan task 9.1) are on by default.** Every provider request carries the kernel tool
  `note` (about 320 tokens of schema; never executed by the host — a host that filters tools by name
  should let it through like `update_plan`). To keep 0.2.81's behavior, set `notesBudgetTokens: 0`
  (Node/WASM) or `notes_budget_tokens: 0` (Python; Rust `RuntimeOptions.notes_budget_tokens:
  Some(0)`); a test that asserts the exact toolset sees `note` unless it does. With notes on,
  the state turn gains a `notes:` block, item reports may carry origin `note` and relations
  `derived_from`, `links_to` and `supersedes`, and `context_lifecycle_reported` may carry
  `notes_written` and `notes_active`. Checkpoints carry `task_state.notes` only when notes exist;
  0.2.81 cannot restore such a checkpoint.
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

`deepstrike-lab gate` decides by DEC-13 by default: success or cost per correct answer
(`cost_per_success`) significantly better, neither significantly worse, over at least 30 pairs. A
gate that used to pass on summary checks alone can now fail, including with "none of these is
significantly better" or "insufficient samples". Pass `--paired none --improve none` for the old
behavior. Baselines that should reproduce pre-0.2.82 runs must turn notes off
(`--policy '{"notesBudgetTokens":0}'` for the context-lifecycle benchmarks).
