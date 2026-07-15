"""I4 (pre_query_memory) rerouted: strict dynamic context control means a proactive pre-turn-1
memory fetch is still single-use retrieval content, not a stable skill — so it lands in
``history`` (an ordinary turn the model sees on turn 1) rather than pinning itself into the
durable ``knowledge`` slot forever. Mirrors node/tests/pre-query-memory.test.ts."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta
from deepstrike.memory.protocols import MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope

RECALL = "PREFETCHED_LONGTERM_FACT"
SCOPE = MemoryScope("agent-prequery", "prefetch")
RECALL_HIT = MemoryRecall(MemoryRecord(
    record_id="record-prefetch", scope=SCOPE, name="prefetch", kind="reference", content=RECALL,
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


class CapturingProvider:
    def __init__(self) -> None:
        self.saw_in_turns = False
        self.saw_in_knowledge = False

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        rendered = repr(context.turns)
        if RECALL in rendered:
            self.saw_in_turns = True
        if RECALL in (context.system_knowledge or ""):
            self.saw_in_knowledge = True
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_pre_query_memory_lands_in_history_not_knowledge():
    provider = CapturingProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=4,
        agent_id="agent-prequery",
        memory_scope=SCOPE,
        dream_store=FakeDreamStore(),
        pre_query_memory=lambda goal: [MemoryQuery(SCOPE, "past facts")],
    ))

    async for _ in runner.run(goal="use the fact"):
        pass

    assert provider.saw_in_turns is True
    assert provider.saw_in_knowledge is False
