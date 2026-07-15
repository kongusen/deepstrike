import pytest

from deepstrike.memory.in_memory_store import InMemoryDreamStore
from deepstrike.memory.agent import select_memories
from deepstrike.memory.protocols import MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope

SCOPE = MemoryScope("tenant-test", "python-store")
def memory(content: str, updated_at: int) -> MemoryRecord:
    return MemoryRecord(
        record_id=f"record-{updated_at}", scope=SCOPE, name=content, kind="project", content=content,
        description=content, provenance=MemoryProvenance(author="host", trust="host_verified"),
        created_at=1, updated_at=updated_at,
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
