"""Strict dynamic context control: a loaded SKILL is method/procedural content reused for the
rest of the run, so its text gets pinned into the durable ``knowledge`` slot (rendered as
``system_knowledge``) in addition to the ordinary tool_result already headed for ``history``.
Mirrors node/tests/knowledge-skill-pin.test.ts."""

import tempfile
from pathlib import Path

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent

SKILL_BODY = "Debug guidance: always reproduce before fixing."


class SkillLoadProvider:
    def __init__(self) -> None:
        self.call = 0
        self.knowledge_snapshots: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.call += 1
        self.knowledge_snapshots.append(context.system_knowledge or "")
        if self.call == 1:
            yield ToolCallEvent(id="s1", name="skill", arguments={"name": "debug"})
            return
        if self.call == 2:
            yield ToolCallEvent(id="s2", name="skill", arguments={"name": "debug"})
            return
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_skill_content_pinned_into_knowledge_once():
    with tempfile.TemporaryDirectory(prefix="ds-knowledge-pin-") as d:
        Path(d, "debug.md").write_text(
            f"---\nname: debug\ndescription: Debug helper\n---\n{SKILL_BODY}"
        )

        provider = SkillLoadProvider()
        runner = RuntimeRunner(RuntimeOptions(
            provider=provider,
            session_log=InMemorySessionLog(),
            execution_plane=LocalExecutionPlane(),
            max_tokens=2048,
            max_turns=6,
            skill_dir=d,
        ))

        async for _ in runner.run(goal="debug it"):
            pass

        assert provider.call >= 3
        assert SKILL_BODY not in provider.knowledge_snapshots[0]
        last = provider.knowledge_snapshots[-1]
        assert SKILL_BODY in last
        assert last.count(SKILL_BODY) == 1
