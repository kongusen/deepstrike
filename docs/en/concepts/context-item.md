# The Context Item Table

Everything in a model's context is an **item**. Where it came from (its origin) gives it four defaults: who can see it (scope), how it renders (fidelity), when it leaves (lease), and who may write it (authority). This page holds only that table and where each column is enforced in code; other pages link here instead of explaining it again.

**Code**: `crates/deepstrike-core/src/context/item.rs` (`ItemOrigin::defaults`), `context/authority.rs`, `context/voice.rs`

## Four columns and where each is enforced

| Column | Type | Enforced in |
|---|---|---|
| Scope | `ItemScope` | A renewal keeps the knowledge whose scope `survives_renewal()` (`context/renewal.rs`); a child run inherits the knowledge whose scope is `inherited_by(inheritance)` (`ContextManager::inheritable_knowledge`) |
| Fidelity | `Fidelity` | Recorded in two places: a history item records its own form (full, or the excerpt a compaction rewrote it to), and the handle table records where a result's body lives (resident, external, paged out, cleared by the provider). The renderer projects a cleared or paged-out result to a one-line handle from the handle table (`context/renderer.rs`); the lifecycle report and the item graph combine the two with `ContextManager::history_fidelity` |
| Lease | `Lease` | `sweep_expired_item_leases` enforces knowledge leases each turn; `sweep_expired_skill_leases` enforces a skill's activation lease and removes its body when it runs out |
| Authority | `Authority` | The table is `admit_write(origin, writer, op)` (`context/authority.rs`; a test pins every cell). A host replacing or removing knowledge is checked against it (`ContextManager::check_host_knowledge_write`); the model's `update_plan` is checked by `admit_task_update` |

## Origin defaults

| origin | scope | fidelity | lease | author |
|---|---|---|---|---|
| `system` | operation | full | permanent | kernel |
| `skill` | task | full | until the skill is deactivated | model |
| `memory_recall` | session | handle | 8 turns without use | host |
| `reference` | operation | full | permanent | host |
| `blackboard` | workflow | full | permanent | host |
| `tool_result` | task | full | permanent | host |
| `dependency_output` | edge | handle | permanent | kernel |
| `user` | operation | full | permanent | host |
| `assistant` | operation | full | permanent | model |
| `provider_summary` | operation | full | permanent | provider |

An item the host pins is exempt from turn limits, but still leaves on the event that ends its purpose (a skill body leaves when the skill is deactivated).

## Authority

| Writer | May |
|---|---|
| kernel | write any item |
| host | create any item; replace or remove any item except those the kernel or the provider wrote |
| model | write only origins the model authors (skill, assistant) |
| provider | write only origins the provider authors (provider_summary) |

`[TASK STATE]` fields: through `update_plan` the model sets only `plan` and `current_step`; the host may also set `directives` (the user's standing commands) and `preserved_refs` (results shielded from compaction). An update is accepted whole or refused whole.

## What is not an item

Signals, the `[TASK STATE]` block and the model's notes render in the state turn but are not stored as items: a signal is a `SignalItem` that leaves when the request carrying it commits; notes live in the task state, and the kernel gives them item ids only to record the edges between notes and results.

## The kernel's words

Every text the kernel writes to the model (signals, notices, the results it answers calls with, refusal reasons, the `[TASK STATE]` block, the descriptions of its own tools) comes from `context/voice.rs`. The lifecycle report's `kernel_text_tokens` counts how much kernel text the model was shown.

## Further reading

- [Context Engineering](/en/guides/context-engineering)
- [Prompt Cache Design](/en/concepts/prompt-cache-design)
- [DEC-13: Model-led context](/en/decisions/013-model-led-context)
