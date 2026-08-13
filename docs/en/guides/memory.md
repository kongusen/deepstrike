# Memory

Memory lets an Agent carry useful facts beyond one conversation. DeepStrike separates scratch memory, session history, and durable records so the Agent can recall what matters without replaying every old message.

**Source code:**
- Runtime: `crates/deepstrike-core/src/memory/`
- SDK: `python/deepstrike/memory/`, `RuntimeRunner.write_memory` / `query_memory`

---

## Three kinds of memory

| Layer | Use |
|-------|--------------|
| Working | Scratch pad for the current run |
| Session | What happened during one run, including tool and Agent activity |
| Durable | Facts, preferences, and project knowledge available in future runs |

Memory is not an automatic transcript archive. Your application chooses what is worth keeping, how it is validated, and how it is retrieved.

![Memory Mechanisms](/memory_mechanisms.svg)

## Concept

| Layer | Description |
|-------|-------------|
| Working | `WorkingMemory` scratch pad |
| Session | Per-run session data |
| Durable | `MemoryStore` persistence + session extraction |

Agents can write and query memory directly through the runner APIs or the built-in `memory` tool.

---

## Level 1: write / query

Implement the `MemoryStore` protocol (`memory/protocols.py`) and pass it to the runner:

```python
class MyStore:
    async def put(self, agent_id, record): ...
    async def get(self, agent_id, record_id): return None
    async def delete(self, agent_id, record_id): ...
    async def save_session(self, data): ...
    async def search(self, agent_id, query): return []

runner = RuntimeRunner(RuntimeOptions(
    ...,
    agent_id="my-agent",
    memory_store=MyStore(),
))

await runner.write_memory({
    "metadata": {
        "name": "prefers-small-tests",
        "description": "User prefers focused unit tests",
        "kind": "feedback",
        "created_at": 1,
        "updated_at": 1,
    },
    "content": "User prefers focused unit tests for SDK behavior.",
}, session_id="s1")

hits = await runner.query_memory({
    "current_context": "Need memory about tests",
    "active_tools": [],
    "already_surfaced": [],
    "top_k": 3,
}, session_id="s1")
```

Reference test: `python/tests/test_memory_syscall.py`

```python
# From test_write_memory_commits_to_memory_store_after_kernel_validation
await runner.write_memory({
    "metadata": {
        "name": "prefers-small-tests",
        "description": "User prefers small focused tests",
        "kind": "feedback",
        "created_at": 1,
        "updated_at": 1,
    },
    "content": "User prefers focused unit tests for SDK behavior.",
}, session_id="memory-syscall-py")
```

---

## Level 2: MemoryPolicy

```python
from deepstrike import MemoryPolicy

RuntimeOptions(
    ...,
    memory_policy=MemoryPolicy(
        validation_enabled=True,
        max_content_bytes=4096,
        max_name_length=64,
        retrieval_top_k=5,
        stale_warning_days=30,
    ),
)
```

On validation failure the runtime emits an observation and **does not commit** to the store.

---

## Level 3: Pre-fetch before run (+ renewal re-query)

```python
def pre_query(goal: str, phase: str | None = None):
    # phase == "initial": the one-shot pre-turn-1 fetch
    # phase == "renewal": auto re-fired after a sprint renewal (the old history —
    #                     including earlier hits — was just dropped)
    return ["user preferences", "project conventions"]

RuntimeOptions(
    ...,
    pre_query_memory=pre_query,
    memory_store=store,
    agent_id="my-agent",
)
```

Before turn 1, searches the durable `MemoryStore`; hits land in **history as an ordinary turn** —
single-use fact content that decays with the compression pyramid, never pinned into the
knowledge partition. A sprint renewal rebuilds history wholesale, so the hook re-fires with
`phase="renewal"`, giving the new sprint a fresh recall pass. Pre-existing hooks that don't
accept `phase` (`lambda goal: [...]`) keep working unchanged.

---

## Level 4: Session extraction

At session completion, the runner saves the transcript and extracts candidate records through the provider or `memory_summarizer`. Each record still returns through the runtime `write_memory` gate before it reaches `MemoryStore`.

SDK configuration:

```python
RuntimeOptions(
    ...,
    memory_provider=synthesis_provider,
    memory_summarizer=custom_summarizer,
    memory_system_prompt="Extract durable insights from sessions...",
)
```

---

## Level 5: Recall journaling & retention

Recall is a scored query with feedback, and forgetting is retention-based eviction. The application's `MemoryStore` owns both.

- **Recall journaling.** When `query_memory` routes a hit, the runtime derives the record's next `recall_count` from that hit and emits a `memory_recalled` observation. The application `MemoryStore.recordRecall` folds it back, so a record that keeps getting recalled accrues usage without the runtime holding the durable ledger.
- **Promotion on threshold.** Crossing `MemoryPolicy.promotion_recall_threshold` emits a `promotion_suggested` observation once, at the crossing. The `onPromotionSuggested` callback lets the application pin a frequently recalled record into durable knowledge.
- **Retention & eviction.** `memory_retention_score` ranks records by usage, kind, confidence, recency, and size (pinned records sort to the top). The application `MemoryStore` uses it to evict cold records to capacity. Forgetting is deterministic ranking, not FIFO.

```python
RuntimeOptions(
    memory_policy=MemoryPolicy(promotion_recall_threshold=3),
    on_promotion_suggested=lambda rec: memory_store.set_pinned(rec.record_id, True),
)
```

Host mirror of the scoring vocabulary: `node/src/memory/retention.ts`, `python/deepstrike/memory/retention.py`.

---

## ResourceQuota write rate limit

```python
from deepstrike import ResourceQuota, MemoryWriteRateLimit

RuntimeOptions(
    ...,
    resource_quota=ResourceQuota(
        memory_writes_per_window=MemoryWriteRateLimit(max_writes=10, window_ms=60_000),
    ),
)
```

---

## Runtime behavior

- `write_memory` validates metadata and content against `MemoryPolicy` before `commit`
- `query_memory` runs search, ranks hits, and surfaces them as runtime observations
- Session extraction runs after completion; SDK synthesis returns through runtime-governed writes

---

## Further reading

- [Context Engineering](./context-engineering) — knowledge partition
- [Governance](./governance) — syscall trap
- `InMemoryMemoryStore` — development implementation
