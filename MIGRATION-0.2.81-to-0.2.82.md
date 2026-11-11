# Migration from 0.2.81 to 0.2.82

0.2.82 is stage 8 of the [context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md):
measure first, then put the cache into decisions.

## Stored state

0.2.82 restores checkpoints written by 0.2.81: a checkpoint without `cache_ledger` restores with an
empty ledger, so the first request after the restore is not compared with anything. Checkpoints
written by 0.2.82 carry the new optional fields `cache_ledger` and `model_summaries`; 0.2.81 cannot
read them.

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
- Rendering, compaction and provider requests are unchanged unless a host opts in to model
  summaries (below). Cache-aware compaction (plan task 8.3) was measured and is not part of this
  release; see the plan.

## Model summaries (opt-in)

- The effect union gains an eleventh kind, `summarize_context`, resolved with the success kind
  `context_summarized` (`{ "summary": "..." }`). It is optional: the kernel publishes it only when
  `host_effect_support` declares it, and no configuration requires it. A host that implements the
  effect protocol directly and matches effect kinds exhaustively must add the new kind to its
  match even if it never declares it.
- To turn it on: Node / WASM `new RuntimeRunner({ ..., modelSummaries: true })` (or
  `{ provider, timeoutMs }`), Python `RuntimeOptions(model_summaries=True)` (or
  `ModelSummaryOptions(provider=..., timeout_ms=...)`), Rust
  `model_summaries: Some(ModelSummaryOptions::default())`. Rust `RuntimeOptions` literals must add
  `model_summaries: None` to keep the old behavior.
- Each summary call costs one provider request whose input is the archived range (at most about
  16k tokens). It is recorded as a `context_summarized` session event (new kind in the session
  event registry); consumers that validate event kinds strictly should accept it.
- `context_lifecycle_reported` gains optional `model_summaries`; item records and `history`
  tallies can carry the origin `summary`. Summaries written by the kernel's own compaction now
  have `summarizes` edges in `edges`, so the edge count of a run that compacts is higher than in
  0.2.81.
- Whether model summaries improve accuracy has not been measured live yet (see plan task 8.4);
  they are off by default until a measurement passes the gate.

## Benchmarks

Trials gain `cost.requests[]` (each request's `input_tokens`, `cache_read_tokens`,
`cache_write_tokens`, `output_tokens`, and `run` when a trial has several runs). Trials recorded by
0.2.81 have no request detail: `deepstrike-lab` shows no `observed_cache_loss_tokens` or
`cache_loss_error` for them, and `gate --max-cache-loss-error` fails a candidate without it.
Trials also gain `model` and `cost.cache_write_tokens`, which `weighted_cost` prices; a trial
without them is priced at the default prices with no cache writes. `gate --paired` needs at least
`--min-pairs` (30) runs of the same tasks under both variants. Trials from runs with model
summaries count each summary call in `cost` and as a request of its own run (`summary:<effect id>`).
