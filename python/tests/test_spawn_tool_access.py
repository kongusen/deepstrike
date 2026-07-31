"""``AgentRunSpec.tool_access`` on the public spawn path (``RuntimeRunner.spawn_sub_agent``).

Under canonical ABI v3 the direct host spawn bypass is unavailable — child work must be authored
through provider workflow meta-tools. Two cases pin the fail-closed boundary:

 (a) ``tool_access="inherit"`` — rejected before any child stream exists.
 (b) the default ("filtered") — also rejected; no zero-tools warning fires because spawn never starts.

Mirrors the Node ``spawn-tool-access.test.ts``. Test (c) still exercises the orchestrator grant seam
directly for workflow-node quarantine (no misconfig warning).
"""
from __future__ import annotations

import warnings

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
)
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime.sub_agent_orchestrator import (
    SubAgentRunContext,
    _resolve_tool_grants,
)
from deepstrike.tools import tool
from deepstrike.types.agent import (
    AgentIdentity,
    AgentProcessChangedObservation,
    AgentRunSpec,
)


class _RecordingProvider:
    """Records the tool names it is handed on every LLM call, then completes the turn with text."""

    def __init__(self) -> None:
        self.calls: list[list[str]] = []

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls.append([t.name for t in tools])
        yield TextDelta(delta="done")


def _noop() -> str:
    """Do nothing."""
    return "ok"


async def _make_parent() -> tuple[RuntimeRunner, _RecordingProvider]:
    """Parent runner over a ``_noop``-bearing plane (no injected kernel — spawn is fail-closed)."""
    provider = _RecordingProvider()
    plane = LocalExecutionPlane()
    plane.register(tool(_noop))
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=plane,
        max_tokens=4096,
        max_total_tokens=100_000,
        agent_id="parent",
    ))
    return runner, provider


_SPAWN_UNAVAILABLE = r"canonical ABI v3"


@pytest.mark.asyncio
async def test_inherit_runs_child_on_parent_plane_without_capability_grant():
    runner, provider = await _make_parent()

    spec = AgentRunSpec(
        identity=AgentIdentity(agent_id="worker", session_id="worker-inherit", is_sub_agent=True),
        role="implement",
        isolation="shared",
        goal="do the work",
        tool_access="inherit",
    )
    with pytest.raises(RuntimeError, match=_SPAWN_UNAVAILABLE):
        async for _event in runner.spawn_sub_agent(spec):
            pass
    assert provider.calls == []


@pytest.mark.asyncio
async def test_default_filtered_zero_tools_warns_but_completes():
    runner, _ = await _make_parent()

    spec = AgentRunSpec(
        identity=AgentIdentity(agent_id="worker", session_id="worker-filtered", is_sub_agent=True),
        role="implement",
        isolation="shared",
        goal="do the work",
        # tool_access omitted ⇒ default "filtered"; no capability_filter ⇒ deny-all.
    )
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always", RuntimeWarning)
        with pytest.raises(RuntimeError, match=_SPAWN_UNAVAILABLE):
            async for _event in runner.spawn_sub_agent(spec):
                pass
    assert not any("zero tools" in str(w.message) for w in caught)


def test_workflow_node_zero_tools_is_exempt_from_warning():
    """A workflow node runs filtered with no grants by design (quarantine deny-all); the misconfig
    warning must NOT fire. Exercises the grant-resolution seam directly (no full workflow driver)."""
    opts = RuntimeOptions(
        provider=_RecordingProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
    )
    ctx = SubAgentRunContext(
        parent_opts=opts,
        parent_session_id="parent",
        spec=AgentRunSpec(
            identity=AgentIdentity(agent_id="wf-node", session_id="parent-wf-node", is_sub_agent=True),
            role="verify",
            isolation="read_only",
            goal="check the untrusted content",
        ),
        manifest=AgentProcessChangedObservation(
            agent_id="wf-node",
            parent_session_id="parent",
            role="verify",
            isolation="read_only",
            context_inheritance="none",
            permitted_capability_ids=[],
        ),
        session_log=opts.session_log,
        is_workflow_node=True,
        tool_access="filtered",
    )
    with warnings.catch_warnings():
        # Promote any RuntimeWarning to an error: a warning here fails the test.
        warnings.simplefilter("error", RuntimeWarning)
        _resolve_tool_grants(ctx)
