"""Plan task 9.4 · consolidation is an ordinary run (mirrors node/tests/consolidate.test.ts): the
model searches earlier transcripts with the host's ``transcript_search`` tool and proposes records
with ``propose_memory``, which reach the host's candidate sink through the candidate ABI."""

import json

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.memory import (
    InMemoryMemoryStore,
    consolidate_memory,
    search_transcripts,
    transcript_lines,
    transcript_search_tool,
)
from deepstrike.memory.candidate_sink import InMemoryMemoryCandidateSink
from deepstrike.memory.protocols import MemoryScope
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool

SCOPE = MemoryScope("agent-memory", "incidents")


class EarlierSession:
    def __init__(self) -> None:
        self.calls = 0

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        if self.calls == 1:
            yield TextDelta(delta="reading the log")
            yield ToolCallEvent(id="c1", name="read", arguments={"path": "logs/svc-03.log"})
            return
        yield TextDelta(delta="req ids ending in 0 are retries and must be skipped")


async def _earlier_session(log: InMemorySessionLog, session_id: str) -> None:
    @tool
    def read(path: str) -> str:
        """Read a file."""
        return "2026-10-02 ERROR code=E503 req=r120 upstream timeout (retry)\n2026-10-02 INFO ok"

    runner = RuntimeRunner(RuntimeOptions(
        provider=EarlierSession(), session_log=log, execution_plane=LocalExecutionPlane().register(read),
        max_tokens=8000, baseline_tool_ids=["read"],
    ))
    async for _ in runner.run(goal="count the E503 errors", session_id=session_id):
        pass


@pytest.mark.asyncio
async def test_transcript_search_finds_tool_results_labeled_with_their_call() -> None:
    log = InMemorySessionLog()
    await _earlier_session(log, "s1")
    lines = transcript_lines("s1", await log.read("s1"))
    assert any(line.source == "goal" and "E503" in line.text for line in lines)
    found = search_transcripts(lines, "E503 retry")
    assert "c1 read" in found[0].source
    assert "code=E503 req=r120" in found[0].text
    search = transcript_search_tool(log, ["s1"])
    assert "[s1 · turn" in await search(query="E503 retry")
    assert await search(query="E503", session_id="other") == "no matching lines"


class Consolidator:
    def __init__(self) -> None:
        self.calls = 0
        self.offered: list[list[str]] = []
        self.seen: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        self.offered.append([t.name for t in tools])
        self.seen.append(json.dumps([str(getattr(turn, "content", "")) for turn in context.turns]))
        if self.calls == 1:
            yield ToolCallEvent(id="t1", name="transcript_search", arguments={"query": "E503 retry"})
            return
        if self.calls == 2:
            yield ToolCallEvent(id="p1", name="propose_memory", arguments={
                "name": "e503-retries", "kind": "project",
                "content": "E503 lines with req ids ending in 0 are retries; skip them (s1, turn 1).",
            })
            return
        yield TextDelta(delta="proposed e503-retries")


@pytest.mark.asyncio
async def test_consolidation_is_an_ordinary_run_whose_proposals_reach_the_sink() -> None:
    log = InMemorySessionLog()
    await _earlier_session(log, "s1")
    provider, sink = Consolidator(), InMemoryMemoryCandidateSink()
    options = RuntimeOptions(
        provider=provider, session_log=log, max_tokens=8000, max_turns=6,
        agent_id="agent-memory", memory_scope=SCOPE, memory_store=InMemoryMemoryStore(),
        memory_candidate_sink=sink,
        memory_runtime_policy={"retrieval": "host", "model_writes": "candidate_sink"},
    )
    result = await consolidate_memory(options, ["s1"])
    assert {"transcript_search", "propose_memory"} <= set(provider.offered[0])
    assert "code=E503 req=r120" in provider.seen[1]
    assert [c.record.content for c in sink.candidates()] == [
        "E503 lines with req ids ending in 0 are retries; skip them (s1, turn 1).",
    ]
    assert result == {"session_id": "consolidate-1", "text": "proposed e503-retries"}
