"""Plan task 1.1 · a workflow node's child run starts with the parent knowledge the kernel chose for it
by item scope (``inherited_knowledge`` on the spawn), keyed and pinned as the parent held it."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.stream import TextDelta


class SeeingProvider:
    def __init__(self):
        self.seen = ""

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.seen = repr(context)
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_inherited_knowledge_renders_in_the_child():
    provider = SeeingProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=4096,
        max_turns=2,
        inherited_knowledge=[{"content": "HOUSE_STYLE forbids bullet lists", "key": "style", "tokens": 6, "pinned": True}],
    ))
    async for _ in runner.run(goal="write the summary", session_id="inherit"):
        pass
    assert "HOUSE_STYLE forbids bullet lists" in provider.seen
