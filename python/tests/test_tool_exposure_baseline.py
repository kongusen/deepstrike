"""Exposure baseline and fail-closed dispatch (mirrors node/tests/tool-gating.test.ts).

``baseline_tool_ids`` is the PRE-ACTIVATION tool surface under the ``allowed_tool_ids`` ceiling, so
the narrow→wide progressive-disclosure shape becomes expressible::

    exposed = meta ∪ ((baseline ∪ stable_core ∪ ⋃ active_skills.allowed_tools) ∩ ceiling)

A call to a tool this run never advertised never reaches the host; the kernel commits a
model-visible ``governance_denied`` result instead.
"""

import tempfile
from pathlib import Path

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


def _context_text(context: RenderedContext) -> str:
    """Everything the model can read out of a rendered context, as one searchable string."""
    parts = [
        context.system_text,
        context.system_stable,
        context.system_knowledge,
        getattr(context.state_turn, "content", None) if context.state_turn else None,
    ]
    for message in context.turns:
        parts.append(message.content)
        # Tool results ride `content_parts`, not `content` — a kernel-synthesized denial lands here.
        for part in getattr(message, "content_parts", None) or []:
            parts.append(getattr(part, "text", None))
            parts.append(getattr(part, "output", None))
    return "\n".join(str(p) for p in parts if p)


def _base_tools() -> list:
    @tool
    def read() -> str:
        """read"""
        return "r"

    @tool
    def write() -> str:
        """write"""
        return "w"

    @tool
    def bash() -> str:
        """bash"""
        return "b"

    @tool
    def grep() -> str:
        """grep"""
        return "g"

    return [read, write, bash, grep]


def _plane(tools: list) -> LocalExecutionPlane:
    plane = LocalExecutionPlane()
    for t in tools:
        plane.register(t)
    return plane


class SkillLoadingProvider:
    """Records the toolset per turn; loads ``skill(debug)`` on turn 1, then finishes."""

    def __init__(self) -> None:
        self.call = 0
        self.per_turn: list[list[str]] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.per_turn.append([t.name for t in tools])
        self.call += 1
        if self.call == 1:
            yield ToolCallEvent(id="s1", name="skill", arguments={"name": "debug"})
            return
        yield TextDelta(delta="done")


class ToolCapturingProvider:
    """Records the toolset of the single turn it takes, then finishes."""

    def __init__(self) -> None:
        self.tools: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.tools = [t.name for t in tools]
        yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_baseline_starts_narrow_and_widens_by_the_activated_skill():
    with tempfile.TemporaryDirectory() as tmp:
        Path(tmp, "debug.md").write_text(
            "---\nname: debug\ndescription: Debug helper\nallowed_tools: write\n---\nDebug guidance."
        )
        provider = SkillLoadingProvider()
        runner = RuntimeRunner(RuntimeOptions(
            provider=provider,
            session_log=InMemorySessionLog(),
            execution_plane=_plane(_base_tools()),
            max_tokens=8000,
            max_turns=6,
            skill_dir=tmp,
            # Ceiling: what this run may EVER expose. `grep` is inside it but never reachable,
            # because neither the baseline nor the skill names it — a bound, not a grant.
            allowed_tool_ids=["read", "write", "grep"],
            # Baseline: the pre-activation surface. `bash` sits OUTSIDE the ceiling ⇒ D3 silent
            # intersection (no start_run error, it simply never appears).
            baseline_tool_ids=["read", "bash"],
        ))

        async for _ in runner.run(goal="debug it", session_id="baseline-widen"):
            pass

        assert len(provider.per_turn) >= 2
        before, after = provider.per_turn[0], provider.per_turn[-1]

        # Turn 1 — narrow: baseline ∩ ceiling = {read}. `write` is reachable-but-not-advertised,
        # exactly the expressiveness `allowed_tool_ids` alone could not deliver.
        assert "read" in before
        assert "write" not in before
        assert "grep" not in before
        assert "bash" not in before  # D3
        assert "skill" in before  # meta stays exempt so the model can widen its own surface

        # Turn 2 — widened by exactly the declaration, still under the ceiling.
        assert "read" in after and "write" in after
        assert "grep" not in after
        assert "bash" not in after


@pytest.mark.asyncio
async def test_empty_baseline_is_the_minimal_surface_distinct_from_unset():
    provider = ToolCapturingProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=_plane(_base_tools()),
        max_tokens=8000,
        max_turns=3,
        baseline_tool_ids=[],
        stable_core_tool_ids=["read"],
        enable_plan_tool=True,
    ))
    async for _ in runner.run(goal="do it", session_id="baseline-minimal"):
        pass

    # `[]` is NOT the `allowed_tool_ids` "empty = no gating" trap: it really means minimal.
    assert "write" not in provider.tools
    assert "bash" not in provider.tools
    # stable-core survives the minimal baseline (a union term of the formula)...
    assert "read" in provider.tools
    # ...and so do the kernel meta surfaces.
    assert "update_plan" in provider.tools


@pytest.mark.asyncio
async def test_unset_baseline_keeps_the_minimal_surface():
    provider = ToolCapturingProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=_plane(_base_tools()),
        max_tokens=8000,
        max_turns=3,
        stable_core_tool_ids=["read"],
        enable_plan_tool=True,
    ))
    async for _ in runner.run(goal="do it", session_id="baseline-unset"):
        pass
    assert provider.tools == ["read", "update_plan"]


class UnexposedCallProvider:
    """Calls the gated-out `write` on turn 1 alongside an exposed sibling, then finishes."""

    def __init__(self) -> None:
        self.call = 0
        self.contexts: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.contexts.append(_context_text(context))
        self.call += 1
        if self.call == 1:
            yield ToolCallEvent(id="c-allowed", name="read", arguments={})
            yield ToolCallEvent(id="c-denied", name="write", arguments={})
            return
        yield TextDelta(delta="done")


def _gated_runner(ran: dict):
    @tool
    def read() -> str:
        """read"""
        ran["read"] = True
        return "r"

    @tool
    def write() -> str:
        """write"""
        ran["write"] = True
        return "w"

    provider = UnexposedCallProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=_plane([read, write]),
        max_tokens=8000,
        max_turns=4,
        # `write` stays REGISTERED on the plane but outside the exposure ceiling.
        allowed_tool_ids=["read"],
        baseline_tool_ids=["read"],
    ))
    return runner, provider


@pytest.mark.asyncio
async def test_fail_closed_dispatch_denies_a_registered_but_unexposed_tool():
    ran = {"read": False, "write": False}
    runner, provider = _gated_runner(ran)
    async for _ in runner.run(goal="do it", session_id="dispatch-closed"):
        pass

    assert ran["write"] is False
    # Allowed siblings in the SAME batch still execute — the gate partitions, it does not abort.
    assert ran["read"] is True
    # The denial is model-visible and says what to do next, so the tool_call is answered rather
    # than orphaned (a bare tool_use block is wire-invalid on strict vendors).
    assert "is not part of this run's toolset" in provider.contexts[-1]
    assert "write" in provider.contexts[-1]
