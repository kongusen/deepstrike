import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, Message, RuntimeOptions, RuntimeRunner, streaming_tool
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent, ToolDeltaEvent, ToolResultEvent


class ToolStreamingProvider:
    def __init__(self):
        self.calls = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="unused")

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        if self.calls == 1:
            yield ToolCallEvent(id="call_1", name="compose", arguments={})
        else:
            yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_streaming_tool_chunks_are_forwarded_and_aggregated():
    async def compose():
        yield "hello"
        yield " "
        yield "world"

    provider = ToolStreamingProvider()
    plane = LocalExecutionPlane().register(streaming_tool(compose))
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=2048,
        max_turns=4,
    ))

    events = [event async for event in runner.run(goal="compose once")]

    assert any(isinstance(e, ToolDeltaEvent) and e.delta == "hello" for e in events)
    assert any(isinstance(e, ToolDeltaEvent) and e.delta == " " for e in events)
    assert any(isinstance(e, ToolDeltaEvent) and e.delta == "world" for e in events)
    assert any(isinstance(e, ToolResultEvent) and e.content == "hello world" for e in events)
