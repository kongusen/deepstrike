# Migration from 0.2.79 to 0.2.80

0.2.80 completes stage 4 (lab, graded evidence), the kernel half of stage 5 (provider context
facts) and stage 6 (blackboard, host program API in every SDK) of the
[context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md).

## Stored state

0.2.80 restores checkpoints written by 0.2.79. Checkpoints written by 0.2.80 carry new optional
fields (`item_replays`, `held_history`, `estimate_samples`, `last_request_estimate`, and
`strong_refs` / `lexical_refs` on knowledge slots); 0.2.79 cannot read them.

## Behavior changes

- Provider effects carry `estimated_tokens` on the rendered context and `item_id` on each rendered
  turn. A host that builds provider requests itself may ignore both.
- A host that executes provider effects itself should attach the provider's replay state
  (reasoning content, native blocks) to the completed message as `replay`. The kernel keeps it with
  the turn and renders it back as `replay` on that turn; providers should use it before any
  content-keyed lookup. The bundled SDKs do this already.
- Long runs on reasoning models compact earlier than before: replay sent back with each turn now
  counts toward context pressure, as it counts toward what the model sees.
- `context_lifecycle_reported` reports knowledge as wasted when nothing but common words tied the
  conversation to it. Item `value` records carry `strong_refs` and `lexical_refs`.
- A memory search the model runs in the same turn as a host tool now places its hits after the
  turn's last tool result instead of between results.
- Dynamic workflow programs (all SDKs) can share state through `ctx.blackboard`. Entries are
  `blackboard:<key>` knowledge items; a host must not write that key prefix for anything else.
- Rust: `RuntimeOptions` gains `inherited_knowledge` (set it to `Vec::new()` where options are built
  field by field).
