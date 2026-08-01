"""Workflow-node capability filtering preserves intentional quarantine deny-all semantics."""
from __future__ import annotations

import warnings

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
)
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime.sub_agent_orchestrator import (
    SubAgentRunContext,
    _resolve_tool_grants,
)
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
