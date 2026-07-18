"""``AgentRunSpec.tool_access`` on the public spawn path (``RuntimeRunner.spawn_sub_agent``).

Before this field the spawn path never set the orchestrator's ``tool_access``, so every spawned
sub-agent ran "filtered"; with no capability mounted the filter resolved to deny-all and the child
model saw "no tools available". Two cases pin the fix:

 (a) ``tool_access="inherit"`` with NO capability mounting — the child runs on the parent's execution
     plane, so its first provider call still carries the parent's ``_noop`` tool and it completes.
 (b) the default ("filtered") with no capability — the child resolves to zero tools; the orchestrator
     emits a host-visible ``RuntimeWarning`` ("zero tools"), and the child still runs to completion
     (the warning is advisory, not fatal).

Mirrors the Node ``spawn-tool-access.test.ts``. The parent kernel is injected (as in
``test_nested_group_vehicle.py``); a recording provider captures the tool names handed to each call.
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
from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.base import Message
from deepstrike.providers.stream import DoneEvent, ErrorEvent, TextDelta
from deepstrike.runtime.kernel_step import kernel_action
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
    """Parent runner over a ``_noop``-bearing plane with an injected, already-started kernel
    (spawn_sub_agent requires a live parent run). No capability is mounted — the two cases exercise
    the un-granted path."""
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
    runtime = KernelRuntime(LoopPolicy(max_tokens=128_000))
    kernel_action(runtime, [], {"kind": "start_run", "task": {"goal": "parent", "criteria": []}})
    runner._active_kernel = runtime
    runner._current_session_id = "parent"
    runner._pending_observations = []
    return runner, provider


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
    events = [event async for event in await runner.spawn_sub_agent(spec)]

    # The child completed cleanly and its FIRST LLM call still saw the parent-plane `_noop`.
    assert not any(isinstance(e, ErrorEvent) for e in events)
    done = next((e for e in events if isinstance(e, DoneEvent)), None)
    assert done is not None
    assert done.status == "completed"
    assert len(provider.calls) > 0
    assert "_noop" in provider.calls[0]


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
    with pytest.warns(RuntimeWarning, match="zero tools"):
        events = [event async for event in await runner.spawn_sub_agent(spec)]

    # The warning is advisory: the child still ran to a clean completion.
    done = next((e for e in events if isinstance(e, DoneEvent)), None)
    assert done is not None
    assert done.status == "completed"


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
