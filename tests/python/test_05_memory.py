"""
05 — WorkingMemory + MockDreamStore + Agent.dream()
"""
import time
import pytest

from deepstrike import WorkingMemory
from deepstrike.memory.protocols import SessionData, MemoryEntry, CurationResult, CurationStats

from conftest import MockDreamStore, make_agent


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
    async def test_empty_initially(self):
        assert await MockDreamStore().load_sessions("a") == []

    async def test_add_session_roundtrip(self):
        s = MockDreamStore()
        now = int(time.time() * 1000)
        s.add_session("a1", SessionData(
            session_id="s1", agent_id="a1", messages=[], metadata=None,
            created_at_ms=now, updated_at_ms=now,
        ))
        assert len(await s.load_sessions("a1")) == 1

    async def test_commit_adds_entries(self):
        s = MockDreamStore()
        await s.commit("a1", CurationResult(
            to_add=[MemoryEntry(text="fact A", score=0.9)],
            to_remove_indices=[],
            stats=CurationStats(insights_processed=1, entries_added=1),
        ), [])
        assert len(await s.load_memories("a1")) == 1

    async def test_commit_removes_by_index(self):
        s = MockDreamStore()
        existing = [
            MemoryEntry(text="old A", score=0.5),
            MemoryEntry(text="old B", score=0.5),
        ]
        await s.commit("a1", CurationResult(
            to_add=[MemoryEntry(text="new C", score=0.8)],
            to_remove_indices=[0],
            stats=CurationStats(insights_processed=1, entries_added=1),
        ), existing)
        final = await s.load_memories("a1")
        assert len(final) == 2
        texts = [m.text for m in final]
        assert "old B" in texts
        assert "new C" in texts
        assert "old A" not in texts

    async def test_search_respects_top_k(self):
        s = MockDreamStore()
        await s.commit("a1", CurationResult(
            to_add=[MemoryEntry(text=f"m{i}", score=0.5) for i in range(5)],
            to_remove_indices=[],
            stats=CurationStats(insights_processed=5, entries_added=5),
        ), [])
        assert len(await s.search("a1", "q", 3)) == 3


class TestAgentDream:
    @pytest.mark.timeout(30)
    async def test_returns_zero_when_no_sessions(self):
        store = MockDreamStore()
        agent = make_agent(dream_store=store, agent_id="dreamer")
        r = await agent.dream("dreamer")
        assert r.sessions_processed == 0

    @pytest.mark.timeout(120)
    async def test_processes_session_and_commits(self):
        from deepstrike._kernel import Message as KernelMessage
        store = MockDreamStore()
        agent_id = "dreamer-2"
        now = int(time.time() * 1000)
        store.add_session(agent_id, SessionData(
            session_id="sess-1", agent_id=agent_id,
            messages=[
                KernelMessage(role="user", content="What is the capital of France?"),
                KernelMessage(role="assistant", content="The capital of France is Paris."),
            ],
            metadata=None,
            created_at_ms=now - 3_600_000,
            updated_at_ms=now - 3_600_000,
        ))
        agent = make_agent(dream_store=store, agent_id=agent_id)
        r = await agent.dream(agent_id, now)
        assert isinstance(r.sessions_processed, int)
        assert isinstance(r.insights_extracted, int)
        assert isinstance(r.entries_added, int)
