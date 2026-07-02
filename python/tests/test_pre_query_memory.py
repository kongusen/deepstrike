"""I4 (pre_query_memory) rerouted: strict dynamic context control means a proactive pre-turn-1
memory fetch is still single-use retrieval content, not a stable skill — so it lands in
``history`` (an ordinary turn the model sees on turn 1) rather than pinning itself into the
durable ``knowledge`` slot forever. Mirrors node/tests/pre-query-memory.test.ts."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta

RECALL = "PREFETCHED_LONGTERM_FACT"


class _MemoryEntry:
    def __init__(self, text: str, score: float) -> None:
        self.text = text
        self.score = score
        self.metadata = None


class FakeDreamStore:
    async def load_sessions(self, agent_id):
        return []

    async def load_memories(self, agent_id):
        return []

    async def commit(self, *args, **kwargs):
        return None

    async def search(self, agent_id, query, top_k=5):
        return [_MemoryEntry(RECALL, 0.9)]

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
        dream_store=FakeDreamStore(),
        pre_query_memory=lambda goal: ["past facts"],
    ))

    async for _ in runner.run(goal="use the fact"):
        pass

    assert provider.saw_in_turns is True
    assert provider.saw_in_knowledge is False
