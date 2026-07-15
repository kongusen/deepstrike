"""K4 — renewal-boundary memory re-query (mirrors node/tests/renewal-memory-requery.test.ts).

A sprint renewal drops the old history — including earlier memory hits — so the runner re-fires
the ``pre_query_memory`` prefetch on the live ``renewed`` observation (``phase="renewal"``).
Hits land in ``history`` as ordinary turns, never in ``knowledge``. Pre-K4 hooks that don't
accept ``phase`` keep working (signature-sniffed).
"""

import json

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool
from deepstrike.memory.protocols import MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope

RECALL = "LONGTERM_FACT_FOR_SPRINT"
SCOPE = MemoryScope("agent-k4", "renewal")
RECALL_HIT = MemoryRecall(MemoryRecord(
    record_id="record-renewal", scope=SCOPE, name="renewal", kind="reference", content=RECALL,
    description="fixture", provenance=MemoryProvenance(author="host", trust="host_verified"),
    created_at=1, updated_at=1, confidence=0.9,
), 0.9, "fixture")


class FakeDreamStore:
    async def upsert(self, *args, **kwargs):
        return None

    async def search(self, agent_id, query: MemoryQuery):
        return [RECALL_HIT]

    async def save_session(self, data):
        return None


@pytest.mark.asyncio
async def test_renewal_refires_prefetch_with_phase_and_lands_in_history():
    phases: list[str | None] = []
    st = {"saw_renewal": False, "saw_recall_after_renewal": False, "call": 0}

    def pre_query(goal: str, phase: str | None = None):
        phases.append(phase)
        if phase == "renewal":
            st["saw_renewal"] = True
        return [MemoryQuery(SCOPE, "relevant facts")]

    class Provider:
        async def complete(self, context, tools, extensions=None):
            raise NotImplementedError

        async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
            st["call"] += 1
            if st["saw_renewal"] and RECALL in repr(context.turns):
                st["saw_recall_after_renewal"] = True
            assert RECALL not in (context.system_knowledge or "")
            if st["call"] <= 10 and not st["saw_recall_after_renewal"]:
                yield ToolCallEvent(id=f"b{st['call']}", name="bulk", arguments={})
                return
            yield TextDelta(delta="done")

    @tool
    def bulk() -> str:
        """Bulk filler output — shrinks after the first renewal so pressure subsides and the
        re-fetched recall line survives to the next render."""
        return "ok" if st["saw_renewal"] else "z" * 400

    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=Provider(),
        session_log=session_log,
        execution_plane=LocalExecutionPlane().register(bulk),
        max_tokens=200,
        max_turns=30,
        agent_id="agent-k4",
        memory_scope=SCOPE,
        dream_store=FakeDreamStore(),
        repeat_fuse=False,
        pre_query_memory=pre_query,
    ))

    async for _ in runner.run(goal="long sprint work", session_id="renewal-requery"):
        pass

    events = await session_log.read("renewal-requery")
    assert any(e.event.get("kind") == "context_renewed" for e in events)

    assert phases[0] == "initial"
    assert "renewal" in phases
    assert st["saw_recall_after_renewal"] is True
