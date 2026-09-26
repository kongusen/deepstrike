"""M5 v2.1: top-level auto-pivot (Python).

Canonical host cutover (Task 20) routes model syscalls through core instead of a host-authored
workflow bootstrap path.
"""

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    LoopResult,
    ModelMessage,
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
        return ModelMessage(role="assistant", content="unused")

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


class _Orchestrator:
  """Canned workflow driver: each authored node returns a result naming its agent id."""

  def __init__(self) -> None:
    self.ran: list[str] = []

  async def run(self, ctx):
    agent_id = ctx.spec.identity.agent_id
    self.ran.append(agent_id)
    return SubAgentResult(agent_id=agent_id, result=LoopResult(
      termination="completed",
      turns_used=1,
      total_tokens_used=1,
      final_message=ModelMessage(role="assistant", content=f"result of {agent_id}"),
    ))


async def _noop(**_kwargs) -> str:
  return ""


@pytest.mark.asyncio
async def test_top_level_start_workflow_drives_the_authored_sub_workflow_and_resumes():
  # Parity with Node `workflow-auto-pivot.test.ts`: the model's `start_workflow` is lowered to the
  # canonical DAG, the kernel drives it, and the agent resumes with the node results in context.
  orch = _Orchestrator()
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
    baseline_tool_ids=["start_workflow"],
  ))

  text = ""
  async for evt in runner.run(goal="explore the topic two ways then synthesize"):
    if isinstance(evt, TextDelta):
      text += evt.delta

  assert sorted(orch.ran) == ["wf-node0", "wf-node1"]
  assert len(provider.contexts) >= 2
  second = provider.contexts[1]
  rendered = "\n".join(filter(None, [
    second.system_text, second.system_stable, second.system_knowledge,
    getattr(second.state_turn, "content", None) if second.state_turn else None,
    *[m.content for m in second.turns if isinstance(m.content, str)],
  ]))
  assert "result of wf-node0" in rendered
  assert "synthesized the sub-workflow results" in text
