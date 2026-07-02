"""K3 — skill deactivation + lease (mirrors node/tests/skill-deactivation.test.ts).

``deactivate_skill()`` (host-driven) re-widens the toolset at the next provider call and drops
the ``skill:<name>`` knowledge pin at the next compaction boundary; ``skill_lease_turns`` does
the same automatically after N turns.
"""

import tempfile
from pathlib import Path

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool

SKILL_BODY = "Debug guidance: always reproduce before fixing."


def _skill_dir(tmp: str) -> str:
    Path(tmp, "debug.md").write_text(
        f"---\nname: debug\ndescription: Debug helper\n---\n{SKILL_BODY}"
    )
    return tmp


class DeactivationProvider:
    def __init__(self) -> None:
        self.call = 0
        self.after_deactivation = ""
        self.after_reactivation = ""
        self.runner: RuntimeRunner | None = None

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.call += 1
        if self.call == 1:
            yield ToolCallEvent(id="s1", name="skill", arguments={"name": "debug"})
            return
        if self.call == 2:
            assert SKILL_BODY in (context.system_knowledge or "")
            assert self.runner is not None
            self.runner.deactivate_skill("debug")
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        if self.call <= 10:
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        if self.call == 11:
            self.after_deactivation = context.system_knowledge or ""
            yield ToolCallEvent(id="s2", name="skill", arguments={"name": "debug"})
            return
        self.after_reactivation = context.system_knowledge or ""
        yield TextDelta(delta="done")


class LeaseProvider:
    def __init__(self) -> None:
        self.call = 0
        self.final_knowledge = ""

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.call += 1
        if self.call == 1:
            yield ToolCallEvent(id="s1", name="skill", arguments={"name": "debug"})
            return
        if self.call <= 10:
            yield ToolCallEvent(id=f"b{self.call}", name="bulk", arguments={})
            return
        self.final_knowledge = context.system_knowledge or ""
        yield TextDelta(delta="done")


def _make_runner(provider, skill_dir: str, **extra) -> RuntimeRunner:
    @tool
    def bulk() -> str:
        """Bulk filler output."""
        return "z" * 240

    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane().register(bulk),
        max_tokens=480,
        max_turns=30,
        skill_dir=skill_dir,
        repeat_fuse=False,
        **extra,
    ))


@pytest.mark.asyncio
async def test_deactivate_skill_unpins_at_boundary_and_reactivation_repins():
    with tempfile.TemporaryDirectory() as tmp:
        provider = DeactivationProvider()
        runner = _make_runner(provider, _skill_dir(tmp))
        provider.runner = runner

        async for _ in runner.run(goal="phase work", session_id="skill-deactivate"):
            pass

        assert SKILL_BODY not in provider.after_deactivation
        assert SKILL_BODY in provider.after_reactivation


@pytest.mark.asyncio
async def test_skill_lease_turns_auto_deactivates():
    with tempfile.TemporaryDirectory() as tmp:
        provider = LeaseProvider()
        runner = _make_runner(provider, _skill_dir(tmp), skill_lease_turns=2)

        async for _ in runner.run(goal="leased skill", session_id="skill-lease"):
            pass

        assert SKILL_BODY not in provider.final_knowledge
