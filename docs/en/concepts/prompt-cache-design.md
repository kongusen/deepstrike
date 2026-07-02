# Prompt Cache Design

DeepStrike does not render context by concatenating messages. It treats the prompt as a cacheable address space. The core goal is: **put per-turn volatile state in an uncached tail and keep long-lived content byte-stable**.

Main implementation entry points:

- `crates/deepstrike-core/src/context/renderer.rs`
- `crates/deepstrike-core/src/context/manager.rs`
- `crates/deepstrike-core/src/context/compression.rs`
- `crates/deepstrike-core/src/mm/handle.rs`
- `node/src/types.ts` / `python/deepstrike/runtime/runner.py` turn metrics

## RenderedContext Slots

`RenderedContext` is the structured prompt shape before provider conversion:

```text
system_stable       identity / stable system prompt
system_knowledge    skill definitions / initial_memory / host-pinned durable knowledge
turns               history (incl. runtime memory-tool hits & prefetch); cacheable prefix
state_turn          TASK STATE + signals + recency footer; volatile tail
```

Rendered shape:

```text
[ system_stable ]       ← stable system block
[ system_knowledge ]    ← knowledge block
[ turns[0..frozen] ]    ← deep cache breakpoint when available
[ turns[frozen..] ]     ← hot history tail
[ state_turn ]          ← rebuilt every render, not part of turns
```

`system_text = system_stable + system_knowledge` exists for providers with one system slot. Anthropic can place cache breakpoints on separate system blocks and message history.

## Why state_turn Is Outside turns

`state_turn` contains:

- `[TASK STATE]`: goal, criteria, plan, blocked_on, compression log
- signals: rollback, interrupt, external events
- salience footer: recent real tool actions, next step, latest directive
- `Proceed.` anchor

These change almost every turn. If they lived in `turns`, the cacheable message prefix would drift every render. Keeping `state_turn` as a volatile tail lets provider adapters place it after history or before history without polluting the reusable prefix.

## PrefixFingerprint

Every render can compute a cache-prefix fingerprint:

```rust
pub struct PrefixFingerprint {
    pub system_stable_hash: u64,
    pub system_knowledge_hash: u64,
    pub turn_hashes: Vec<u64>,
}
```

It hashes only provider-wire cache material:

| Included | Excluded |
|----------|----------|
| `system_stable` | `state_turn` |
| `system_knowledge` | `token_count` metadata |
| each history turn's role / content / tool_calls | runtime-only statistics |

`extends(prev)` means the current prefix is only a byte-stable extension of the previous one. If a middle turn is rewritten in place, `common_turn_prefix(prev)` shrinks and cache reuse is lost from that point onward.

## frozen_prefix_len

`ContextManager` maintains `frozen_history_len` after compaction / renewal. Rendering translates it into `RenderedContext.frozen_prefix_len`:

```text
history before compaction boundary  → frozen prefix
history after boundary              → hot tail
```

When there is a non-empty frozen region and a hot tail after it, providers can place a deep cache breakpoint at that boundary. This avoids losing the deep prefix on heavy tool turns that push recent blocks beyond a rolling lookback window.

Before the first compaction, or when there is no distinct frozen region, `frozen_prefix_len = None` and providers fall back to rolling breakpoint placement.

## HandleTable and Read-Time Projection

When a large tool result enters history, `ContextManager.push_history` creates a handle:

```rust
Handle {
    kind: HandleKind::ToolResult,
    residency: Residency::Resident,
    tokens,
    source: Some(call_id),
}
```

The handle's `Residency` controls render-time projection:

| Residency | Behavior |
|-----------|----------|
| `Resident` | full content remains in working context |
| `Collapsed` | original stays in history, rendered copy becomes a preview |
| `SpooledOut` | SDK persists full result, context keeps preview / ref |
| `PagedOut` | content is archived to a memory tier |

`Collapsed` is non-destructive: stored history remains full, while the rendered copy shrinks. Old tool results can leave the prompt without losing recoverable data.

## Compaction Layers and Cache Cost

Compactors return:

```rust
pub struct CompressResult {
    pub tokens_saved: u32,
    pub summary: Option<String>,
    pub archived: Vec<Message>,
    pub prefix_invalidated_at: Option<usize>,
}
```

`prefix_invalidated_at` is the earliest history index rewritten or removed:

| Value | Meaning |
|-------|---------|
| `None` | prefix-safe; cacheable prefix was not rewritten |
| `Some(0)` | prefix broken from the earliest message |
| `Some(n)` | cache invalidated from history message n onward |

The pipeline takes the minimum invalidation index across stages. `ContextManager` re-anchors `frozen_history_len` only when the prefix was actually broken.

## When Compression Runs

Pressure comes from `PressureMonitor`:

- raw `rho()`: decides whether to enter a compression tier
- `effective_rho()`: estimate path subtracts non-resident handle tokens for paging-aware pressure
- provider usage can override estimated prompt token count

Current layers:

| Layer | Behavior | Cache impact |
|-------|----------|--------------|
| Snip | truncate oversized text messages | may rewrite a middle turn |
| MicroCompact | summarize / excerpt large tool results | usually later and more prefix-safe |
| ContextCollapse | drop oldest messages to target | prefix break |
| AutoCompact | keep only recent K turns | prefix break |
| TimeDecayMicro | micro-compact after idle time | independent of pressure tier |

## SDK Observability

Python:

```python
RuntimeOptions(
    ...,
    on_turn_metrics=lambda m: print(m.cache_read_tokens),
)
```

Observable fields include:

- `input_tokens`
- `cache_read_tokens`
- `cache_creation_tokens`
- `cache_read_tokens_by_slot`
- `tools_exposed`
- `tools_called`

Anthropic adapters can attribute cache reads by slot. OpenAI-family automatic caching may not expose equivalent slot data.

## Practices

1. **Keep `system_prompt` stable**: system drift invalidates the whole prefix.
2. **Load Skill bodies on demand**: avoid frequent `system_knowledge` churn.
3. **Use `allowed_tool_ids` static profiles**: stable tool schemas help cache reuse.
4. **Avoid rewriting early history in place**: append is more cache-friendly than rewrite.
5. **Route large tool results through handles / spool / collapse**: do not keep huge outputs resident in the prompt.
6. **Put dynamic state in task_state / signals**: let it enter `state_turn`, not cacheable history.

## Further Reading

- [Context Engineering](/en/guides/context-engineering)
- [Execution Model](/en/architecture/execution-model)
- [Kernel ABI](/en/architecture/kernel-abi)
