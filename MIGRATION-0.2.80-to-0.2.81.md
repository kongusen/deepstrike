# Migration from 0.2.80 to 0.2.81

0.2.81 is stage 7 of the [context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md):
the item graph.

## Stored state

0.2.81 restores checkpoints written by 0.2.80: a knowledge slot without an `id` takes the next item
ids on restore, in slot order. Checkpoints written by 0.2.81 carry new optional fields (`id` on
knowledge slots, `item_edges`, `item_edges_omitted`, `item_external_edges`); 0.2.80 cannot read
them.

## Behavior changes

- Item ids: knowledge items now draw from the same allocator as history items, so history item ids
  in a run are no longer consecutive. Nothing should rely on them being consecutive; ids stay
  unique and increasing.
- `context_lifecycle_reported` gains optional `edges`, `edges_omitted` and `waste_items`, and
  knowledge item records carry `id`. A consumer that validates the event strictly should accept
  them. The edge relations are `answers`, `references_strong`, `references_lexical`, `reads_back`
  and `summarizes`.
- Retention decisions read the graph (plan task 7.3). A knowledge item mentioned only by host tool
  output or by memory search hits no longer counts as used: its turn lease runs out and the
  knowledge budget evicts it first, where 0.2.80 kept renewing it. An item a resident history item
  names (its key, its record id in tool arguments) or read back stays, whatever its lease. A host
  that relied on tool output keeping a recall alive should pin it or name its key.
- `context_lifecycle_reported` gains optional `external_edges` and `parent_items_used`, and edges
  gain the relation `expands` (a memory hit → the recall it gives in full); external edges use
  `inherited_from` and `depends_on`.
- Workflow spawns' `inherited_knowledge` entries carry `inherited_from { operation, item }`, and
  `KnowledgeEntry` (initial context, `seed_knowledge`, knowledge mutations) accepts it. A host that
  launches child runs itself should pass it through; without it the child just reports no
  `inherited_from` edge.
- Python: knowledge seeded into the initial context keeps `pinned`.
