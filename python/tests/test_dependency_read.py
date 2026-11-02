"""Plan task 3.1 · a workflow node's child run holds its dependencies' outputs in the kernel and reads
one in full with ``read_result(call_id="dep:<agent_id>")``, with no payload store involved."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.stream import TextDelta, ToolCallEvent


class ReadingProvider:
    def __init__(self):
        self.turn = 0
        self.tools: list[str] = []
        self.seen = ""

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.turn += 1
        if self.turn == 1:
            self.tools = [getattr(tool, "name", None) or tool.get("name") for tool in tools or []]
            yield ToolCallEvent(id="r1", name="read_result", arguments={"call_id": "dep:wf-node0"})
            return
        self.seen = repr(context.turns)
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_dependency_output_is_read_through_read_result():
    provider = ReadingProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=4096,
        max_turns=4,
        dependency_outputs={"wf-node0": "FULL_SURVEY_BODY with every finding"},
    ))
    async for _ in runner.run(goal="synthesize", session_id="dep-read"):
        pass
    assert "read_result" in provider.tools
    assert "[PAYLOAD handle_id=dep:wf-node0]" in provider.seen
    assert "FULL_SURVEY_BODY with every finding" in provider.seen
