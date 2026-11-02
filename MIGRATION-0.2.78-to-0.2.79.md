# Migration from 0.2.78 to 0.2.79

0.2.79 finishes task 1.1 of the
[context-lifecycle-first plan](./docs/plans/context-lifecycle-first.md): every piece of context is
an item, and an item's scope, fidelity, lease and authority now change what the kernel does.

## Stored state

0.2.79 restores checkpoints written by 0.2.78. Checkpoints written by 0.2.79 carry new optional
fields (`item` on a handle, `fidelity` on a history item, `retired_history`,
`retired_history_omitted`, and the `until` lease on a skill body); 0.2.78 cannot read them.

## Behavior changes

- A host knowledge command (`seed_knowledge`, `apply_knowledge_mutation`; `pushKnowledge`,
  `removeKnowledge` in the SDKs) is refused whole with `InvalidAuthority` when it would overwrite or
  remove an item the kernel wrote (for example `governance:denied-tools`), or remove the body of an
  active skill (`skill:<name>`). Deactivate the skill instead; its body then leaves at the next
  boundary. Removing the body of a skill that is no longer active is still allowed.
- A skill body leaves when its skill is deactivated even if it was pushed with `pinned: true`. A
  pin keeps an item past a time limit (turns, a boundary, a first use), not past the event that
  ends its purpose. The lifecycle report records such an item's end as `expired`, not `removed`.
- A workflow node spawned with `context_inheritance: "full"` starts with the parent's knowledge
  that its scope allows: host reference material and recalled memory index lines, keyed and pinned
  as the parent held them. Active skill bodies and kernel-written items are not inherited. The
  kernel sends them as `inherited_knowledge` on the spawn; a host that starts child runs itself
  should pass them to the child (`inheritedKnowledge` / `inherited_knowledge` option). Nodes with
  `system_only` or `none` inheritance get nothing new.
- `context_lifecycle_reported` adds `history_items`, the latest 128 history items one by one (id,
  origin, tokens, the fidelity they rendered, and why they left: `compacted`, `renewed` or
  `removed`), and `history_items_omitted`, the count of older ones. Item records can now end with
  `compacted` or `renewed`.
- A rollback now drops the measurements, item ids and token count of the rolled-back history
  messages along with the messages. Before, the history token count stayed inflated after a
  rollback.
