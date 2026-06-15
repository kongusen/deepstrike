"""M5 v2.1: top-level auto-pivot (Python).

When a TOP-LEVEL agent (not a workflow node) calls the ``start_workflow`` tool mid-conversation, the
runner records the authored spec, drives it in its own (real) kernel at the safe point (after the tool
turn resolves → kernel back in Reason, not suspended), injects the outcome into context, and resumes the
reason loop. Pure SDK — no kernel change. Exercises the real native kernel end-to-end.
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
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent


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
    """Mock workflow driver: each authored node returns a canned completion (no real LLM)."""

    def __init__(self) -> None:
        self.ran: list[str] = []

    async def run(self, ctx):
        agent_id = ctx.spec.identity.agent_id
        self.ran.append(agent_id)
        return SubAgentResult(
            agent_id=agent_id,
            result=LoopResult(
                termination="completed",
                turns_used=1,
                total_tokens_used=1,
                final_message=Message(role="assistant", content=f"result of {agent_id}"),
            ),
        )


@pytest.mark.asyncio
async def test_top_level_start_workflow_auto_pivots_and_resumes():
    orch = _Stub()
    provider = AuthoringProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=8000,
        max_turns=5,
        # is_workflow_node defaults False ⇒ top-level run ⇒ start_workflow auto-pivots.
    ))

    text = ""
    async for evt in runner.run(goal="explore the topic two ways then synthesize"):
        if isinstance(evt, TextDelta):
            text += evt.delta

    # The authored sub-workflow ran both nodes in THIS kernel (no separate child kernel).
    assert sorted(orch.ran) == ["wf-node0", "wf-node1"]
    # The agent got a 2nd turn AFTER the workflow, carrying the injected outcome in context.
    assert len(provider.contexts) >= 2
    ctx = provider.contexts[1]
    blob = "\n".join(filter(None, [
        ctx.system_text, ctx.system_stable, ctx.system_knowledge,
        getattr(ctx.state_turn, "content", None) if ctx.state_turn else None,
        *[m.content for m in ctx.turns if isinstance(m.content, str)],
    ]))
    assert "[authored workflow result]" in blob
    assert "result of wf-node0" in blob
    # The run continued past the authoring turn and produced the final synthesis text.
    assert "synthesized the sub-workflow results" in text
