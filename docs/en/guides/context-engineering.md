# Context Engineering

Context engineering is the Agent OS **Context VM plane**. It does not simply concatenate messages; it turns identity, knowledge, history, and ephemeral state into a renderable, compressible, cache-aware, pageable working set.

**Source code:** `crates/deepstrike-core/src/context/` (`ContextManager`, `renderer`, `compression`)

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the kernel | Provides deterministic rendered context before each `CallLLM` |
| To providers | Keeps stable prefixes for better prompt-cache reuse |
| To memory / skills / signals | Places durable knowledge, loaded capabilities, and external events into separate slots |
| To tool results | Uses handles / spool residency so large outputs do not flood context |

In OS terms, the Context VM is the agent's virtual memory manager: it decides what stays inline, what is archived, and what is injected only as next-turn state.

![Context VM & Compaction Mechanisms](/context_vm_mechanisms.svg)

## Concept

`RenderedContext` uses four slots:

| Slot | Contents | Cache strategy |
|------|----------|----------------|
| `system_stable` | Identity / system prompt | Long-lived cache |
| `system_knowledge` | Skill body, `initial_memory`, host-pinned durable refs (see Level 5) | Medium-term cache |
| `turns` | Conversation history (incl. runtime memory/knowledge retrieval hits & prefetch) | Frozen prefix, growing tail |
| `state_turn` | task_state + signals | Rebuilt each turn, not cached |

`state_turn` is separated from history so the history prefix stays **byte-stable** — a requirement for Anthropic prompt cache.

```rust
// crates/deepstrike-core/src/context/renderer.rs
pub struct RenderedContext {
    pub system_stable: String,
    pub system_knowledge: String,
    pub turns: Vec<TurnBlock>,
    pub state_turn: String,
    pub frozen_prefix_len: Option<usize>,
}
```

---

## Level 1: Set token limits only

```python
RuntimeOptions(
    provider=provider,
    session_log=session_log,
    max_tokens=32_000,   # context window
    max_turns=25,
)
```

The kernel `PressureMonitor` triggers `CompressionPipeline` (Snip → Drop → Summarize) when pressure exceeds the threshold.

---

## Level 2: System prompt and initial memory

```python
RuntimeOptions(
    ...,
    system_prompt="You are a code review assistant.",
    initial_memory=["User preference: concise answers"],
)
```

`initial_memory` is written into the knowledge partition and injected at run start.

---

## Level 3: Compression archive + large-result paging

```python
from deepstrike.runtime.archive import ArchiveStore

RuntimeOptions(
    ...,
    compression_store=ArchiveStore("./archives"),
    result_spool=large_result_spool,  # Layer-1 spool for oversized tool results
)
```

The handle table (`mm/handle.rs`) projects tool results by residency — hot data stays inline, cold data is paged out without mutating the original partition.

---

## Level 4: Prompt cache fingerprint

Each render pass produces a `PrefixFingerprint` (`renderer.rs`):

- `system_stable_hash` / `system_knowledge_hash`
- `turn_hashes[]` — prefix match means cache is reusable

Observe `cache_read_tokens` via `RuntimeOptions.on_turn_metrics`:

```python
def on_metrics(m):
    print(m.turn, m.cache_read_tokens, m.active_skill)

RuntimeOptions(..., on_turn_metrics=on_metrics)
```

See [Prompt Cache Design](/en/concepts/prompt-cache-design) for slot boundaries and `frozen_prefix_len`.

---

## Level 5: Knowledge lifecycle (strict dynamic control)

The `knowledge` partition is the **durable slot** (rendered into `system[1]` behind a cache
breakpoint) — but durable ≠ immortal. Content routes by lifecycle:

| Content type | Destination | Lifecycle |
| ------ | ------ | ------ |
| Skill body (method content, reused all run) | `knowledge`, key `skill:<name>` | Resident until deactivated / lease expiry |
| `memory` / `knowledge` tool retrieval hits (facts, use-once) | `history` as an ordinary turn | Decays with the compression pyramid |
| `pre_query_memory` prefetch | `history` as an ordinary turn | Same; auto re-queried after renewal |
| Host-pinned reference material | `knowledge` (`push_knowledge(key=..., pinned=True)`) | Pinned — exempt from budget eviction |

**Keyed entries (K1):** `push_knowledge(content, key="ref")` upserts on a repeated key;
`remove_knowledge("ref")` removes by key. Both are **deferred to the next compaction/renewal
boundary** — existing `system[1]` bytes only change at the moment the prompt-cache prefix is
being rewritten anyway (appends are exempt: they merely extend the cached prefix and are visible
immediately).

**Knowledge budget (K2):** `knowledge_budget_ratio` (default 0.25 × max_tokens, 0 disables).
Exceeding it emits one `knowledge_budget_exceeded` observation per cache generation and marks
the **oldest unpinned, non-skill** entries for boundary eviction; pinned entries and skill pins
are never budget-evicted.

**Skill deactivation / lease (K3):** `deactivate_skill(name)` (host-driven only — no model-facing
unload) re-widens the toolset at the next provider call and unpins the knowledge entry at the
next boundary; `skill_lease_turns=N` walks the same path automatically after N turns. Long
multi-phase runs no longer monotonically accumulate early-phase skills.

**Renewal memory re-query (K4):** a sprint renewal rebuilds history wholesale — dropping earlier
memory hits with it — so the `pre_query_memory` hook re-fires with `phase="renewal"`, giving the
new sprint a fresh recall pass (the turn-1 fetch passes `phase="initial"`).

```python
RuntimeOptions(
    ...,
    knowledge_budget_ratio=0.2,   # K2: knowledge may occupy at most 20% of the window
    skill_lease_turns=8,          # K3: skills auto-deactivate 8 turns after activation
)
runner.push_knowledge("API cheat sheet…", key="api-ref", pinned=True)  # K1: keyed + pinned
runner.remove_knowledge("api-ref")                                      # dropped at next boundary
runner.deactivate_skill("debug")                                        # K3: explicit unload
```

---

## Kernel behavior summary

1. **Compression:** `SnipCompactor` truncates oversized messages → `DropCompactor` drops old turns → `SummarizeCompactor` LLM summary (SDK-side summarizer)
2. **Renewal:** Very long runs can hand off via `HandoffArtifact`
3. **Meta-tool exclusion:** `skill`, `memory`, `submit_workflow_nodes`, etc. are excluded from the progress footer

---

## Further reading

- [Execution Plane & Tools](./execution-plane-and-tools) — large tool results, spool, and handle projection
- [Skills](./skills) — `active_skills` narrows exposed tools
- [Memory](./memory) — knowledge partition injection
- Source: `context/manager.rs`, `context/renderer.rs`
