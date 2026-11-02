# Migration from 0.2.80 to 0.2.81

0.2.81 is stage 7 of the [context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md):
the item graph. This document grows with each task of the stage.

## Stored state

0.2.81 restores checkpoints written by 0.2.80: a knowledge slot without an `id` takes the next item
ids on restore, in slot order. Checkpoints written by 0.2.81 carry new optional fields (`id` on
knowledge slots, `item_edges`, `item_edges_omitted`); 0.2.80 cannot read them.

## Behavior changes

- Item ids: knowledge items now draw from the same allocator as history items, so history item ids
  in a run are no longer consecutive. Nothing should rely on them being consecutive; ids stay
  unique and increasing.
- `context_lifecycle_reported` gains optional `edges`, `edges_omitted` and `waste_items`, and
  knowledge item records carry `id`. A consumer that validates the event strictly should accept
  them. The edge relations are `answers`, `references_strong`, `references_lexical`, `reads_back`
  and `summarizes`.
- Rendering, compaction, renewal and every retention decision are unchanged: the graph is reported
  and checkpointed, not read.
