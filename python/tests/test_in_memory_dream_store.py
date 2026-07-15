import pytest

from deepstrike.memory.in_memory_store import InMemoryDreamStore
from deepstrike.memory.agent import select_memories
from deepstrike.memory.protocols import (
    MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecallLifecycle, MemoryRecord, MemoryScope,
)
from deepstrike.memory.retention import memory_retention_score

SCOPE = MemoryScope("tenant-test", "python-store")
def memory(content: str, updated_at: int, *, record_id: str | None = None, recall_count: int = 0,
           confidence: float = 0.9, pinned: bool = False, ttl_days: int | None = None) -> MemoryRecord:
    return MemoryRecord(
        record_id=record_id or f"record-{updated_at}", scope=SCOPE, name=content, kind="project", content=content,
        description=content, provenance=MemoryProvenance(author="host", trust="host_verified"),
        created_at=1, updated_at=updated_at, recall_count=recall_count, confidence=confidence,
        pinned=pinned, ttl_days=ttl_days,
    )


@pytest.mark.asyncio
async def test_search_uses_query_and_never_falls_back_to_unrelated_entries():
    store = InMemoryDreamStore([
        memory("database migration checklist", 1),
        memory("scheduler fairness in Rust", 2),
        memory("newer unrelated note", 3),
    ])

    assert [hit.record.content for hit in await store.search("agent", MemoryQuery(SCOPE, "scheduler Rust"))] == [
        "scheduler fairness in Rust"
    ]
    assert await store.search("agent", MemoryQuery(SCOPE, "nonexistent")) == []


@pytest.mark.asyncio
async def test_selector_ranks_the_query_instead_of_taking_fifo():
    records = [memory("database migration", 1), memory("scheduler fairness", 2)]
    retrieval = await select_memories(MemoryQuery(SCOPE, "scheduler"), records)
    assert [hit.record.content for hit in retrieval] == ["scheduler fairness"]


# M3-C: recall score is relevance, not stored confidence (deviation 1).
@pytest.mark.asyncio
async def test_score_is_relevance_not_confidence():
    store = InMemoryDreamStore([
        memory("token rotation and token expiry", 20, record_id="hi", confidence=0.1),
        memory("refresh token expires in UTC", 10, record_id="lo", confidence=0.99),
    ])
    hits = await store.search("a1", MemoryQuery(SCOPE, "token expiry rotation", top_k=2))
    assert hits[0].record.record_id == "hi"
    assert hits[0].score > 0.1  # unrelated to the 0.1 confidence it was stored with
    assert hits[0].score > hits[1].score
    assert "lexical" in hits[0].why


# M3: value-ordered retention eviction replaces the blind tail-cut; pins are exempt.
@pytest.mark.asyncio
async def test_eviction_sheds_lowest_value_and_never_pinned():
    store = InMemoryDreamStore(max_records=2)
    await store.upsert("a1", memory("cold", 1, record_id="cold", recall_count=0))
    await store.upsert("a1", memory("warm", 1, record_id="warm", recall_count=5))
    await store.upsert("a1", memory("new", 1, record_id="new", recall_count=1))
    ids = {hit.record.record_id for hit in await store.search("a1", MemoryQuery(SCOPE, "cold warm new", top_k=9))}
    assert "cold" not in ids
    assert {"warm", "new"} <= ids

    pinned = InMemoryDreamStore(max_records=1)
    await pinned.upsert("a1", memory("pinned", 1, record_id="pinned", recall_count=0, pinned=True))
    await pinned.upsert("a1", memory("hot", 1, record_id="hot", recall_count=9))
    kept = {hit.record.record_id for hit in await pinned.search("a1", MemoryQuery(SCOPE, "pinned hot", top_k=9))}
    assert "pinned" in kept and "hot" not in kept


# M3/M4: recall + pin lifecycle mirrored from kernel observations.
@pytest.mark.asyncio
async def test_record_recall_and_set_pinned_mirror_into_the_store():
    store = InMemoryDreamStore([memory("a-fact", 1, record_id="rid")])
    await store.record_recall("a1", [MemoryRecallLifecycle(record_id="rid", recall_count=3, last_recalled_at=42)])
    hit = (await store.search("a1", MemoryQuery(SCOPE, "a-fact", top_k=1)))[0]
    assert hit.record.recall_count == 3
    assert hit.record.last_recalled_at == 42
    await store.set_pinned("a1", "rid", True)
    assert (await store.search("a1", MemoryQuery(SCOPE, "a-fact", top_k=1)))[0].record.pinned is True


# Parity: the host retention formula matches the kernel reference for the terms both compute.
def test_retention_score_parity_with_kernel_reference():
    # confidence 0 isolates structural terms: project kind 1400, tokens=100//4=25 → size 100.
    assert memory_retention_score(memory("x" * 100, 0, confidence=0.0), 0, 2) == 1300
    # recall_count 3 → usage_bucket floor(log2(4))=2 → 16384; 16384+1400-100 = 17684.
    assert memory_retention_score(memory("x" * 100, 0, confidence=0.0, recall_count=3), 0, 2) == 17684
    assert memory_retention_score(memory("x", 0, pinned=True), 0, 2) == float("inf")
