"""M5 v2.1: top-level auto-pivot (Python).

Canonical host cutover (Task 20) routes model syscalls through core instead of a host-authored
workflow bootstrap path.
"""

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    LoopResult,
    Message,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
)
from deepstrike._kernel import ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import RegisteredTool
from deepstrike.types.agent import start_workflow_tool


class AuthoringProvider:
    """Emits a ``start_workflow`` tool call on turn 1, then plain text (terminates) afterwards."""

    def __init__(self) -> None:
        self.calls = 0
        self.contexts: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="unused")

    async def stream(self, context, tools, extensions=None, state=None):
        self.contexts.append(context)
        self.calls += 1
        if self.calls == 1:
            yield ToolCallEvent(id="call-1", name="start_workflow", arguments={"spec": {"nodes": [
                {"task": "explore A", "role": "implement"},
                {"task": "explore B", "role": "implement"},
            ]}})
        else:
            yield TextDelta(delta="synthesized the sub-workflow results")


class _Stub:
    async def run(self, ctx):
        raise AssertionError("workflow nodes must not run under canonical auto-pivot cutover")


async def _noop(**_kwargs) -> str:
    return ""


@pytest.mark.asyncio
async def test_top_level_start_workflow_does_not_auto_pivot_under_canonical_host():
    orch = _Stub()
    provider = AuthoringProvider()
    plane = LocalExecutionPlane().register(RegisteredTool(_noop, ToolSchema(
        name=start_workflow_tool["name"],
        description=start_workflow_tool["description"],
        parameters=start_workflow_tool["parameters"],
    )))
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        sub_agent_orchestrator=orch,
        max_tokens=8000,
        max_turns=5,
    ))

    text = ""
    async for evt in runner.run(goal="explore the topic two ways then synthesize"):
        if isinstance(evt, TextDelta):
            text += evt.delta

    # Canonical host does not drive the authored sub-workflow in-run.
    assert "synthesized the sub-workflow results" in text
    assert not any("result of wf-node" in (
        "\n".join(filter(None, [
            ctx.system_text, ctx.system_stable, ctx.system_knowledge,
            getattr(ctx.state_turn, "content", None) if ctx.state_turn else None,
            *[m.content for m in ctx.turns if isinstance(m.content, str)],
        ]))
    ) for ctx in provider.contexts)
