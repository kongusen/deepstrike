"""
05 — WorkingMemory + MockDreamStore
"""
from deepstrike import WorkingMemory
from deepstrike.memory.protocols import (
    MemoryProvenance, MemoryQuery, MemoryRecord, MemoryScope,
)

from conftest import MockDreamStore

SCOPE = MemoryScope("test", "root-memory")
def memory(content: str, confidence: float = 0.5) -> MemoryRecord:
    return MemoryRecord(
        record_id=f"record-{content}", scope=SCOPE, name=content, kind="project", content=content,
        description=content, provenance=MemoryProvenance(author="host", trust="host_verified"),
        created_at=1, updated_at=1, confidence=confidence,
    )


class TestWorkingMemory:
    def test_stores_and_retrieves(self):
        m = WorkingMemory()
        m.set("count", 42)
        assert m.get("count") == 42

    def test_returns_none_for_missing(self):
        assert WorkingMemory().get("x") is None

    def test_returns_default_for_missing(self):
        assert WorkingMemory().get("x", "default") == "default"

    def test_clear_removes_everything(self):
        m = WorkingMemory()
        m.set("a", 1)
        m.set("b", 2)
        m.clear()
        assert m.get("a") is None

    def test_overwrite_replaces_value(self):
        m = WorkingMemory()
        m.set("k", "first")
        m.set("k", "second")
        assert m.get("k") == "second"


class TestMockDreamStore:
    async def test_upsert_adds_entries(self):
        s = MockDreamStore()
        await s.upsert("a1", memory("fact A", 0.9))
        assert len(await s.search("a1", MemoryQuery(scope=SCOPE, query="fact", top_k=5))) == 1

    async def test_search_respects_top_k(self):
        s = MockDreamStore()
        for i in range(5):
            await s.upsert("a1", memory(f"m{i}"))
        assert len(await s.search("a1", MemoryQuery(scope=SCOPE, query="q", top_k=3))) == 3
