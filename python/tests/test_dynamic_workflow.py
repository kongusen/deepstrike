"""Plan task 6.3 · dynamic workflows in Python: a host program runs agents one decision at a time on
a kernel-owned dynamic workflow root, and shares state with them through the blackboard (6.2)."""

import pytest

from deepstrike import InMemorySessionLog, RuntimeOptions, RuntimeRunner
from deepstrike._kernel import ModelMessage
from deepstrike.runtime.dynamic_workflow import (
  DynamicWorkflowAgentOptions,
  DynamicWorkflowLimitError,
  DynamicWorkflowLimits,
  agent_task,
)
from deepstrike.types.agent import LoopResult, SubAgentResult


class _Orchestrator:
  def __init__(self) -> None:
    self.seen: dict[str, list[str]] = {}

  async def run(self, ctx):
    goal = ctx.spec.goal.split("\n")[0]
    self.seen[goal] = [f"{e.get('key')}={e.get('content')}" for e in (ctx.inherited_knowledge or [])]
    return SubAgentResult(agent_id=ctx.spec.identity.agent_id, result=LoopResult(
      termination="completed", turns_used=1, total_tokens_used=1,
      final_message=ModelMessage(role="assistant", content=f"done: {goal}"),
    ))


def _runner(orchestrator: _Orchestrator) -> RuntimeRunner:
  class _Provider:
    async def complete(self, *a, **k):
      raise NotImplementedError

    async def stream(self, *a, **k):
      raise NotImplementedError
      yield

  return RuntimeRunner(RuntimeOptions(
    provider=_Provider(), session_log=InMemorySessionLog(), max_tokens=8000,
    sub_agent_orchestrator=orchestrator,
  ))


@pytest.mark.asyncio
async def test_a_program_runs_agents_and_shares_a_blackboard():
  orchestrator = _Orchestrator()

  async def program(ctx):
    first = await ctx.agent("survey the field")
    await ctx.blackboard.put("criteria", "primary sources only")
    async with ctx.phase("draft"):
      await ctx.blackboard.put("outline", {"sections": 3}, scope="phase")
      pair = await ctx.parallel_agents([agent_task("draft part one"), agent_task("draft part two")])
    last = await ctx.agent("review", DynamicWorkflowAgentOptions(label="reviewer"))
    return [first.text, *(r.text for r in pair), last.node_id, ctx.blackboard.get("outline")]

  run = await _runner(orchestrator).run_dynamic_workflow(program, session_id="dw")
  assert run.value == ["done: survey the field", "done: draft part one", "done: draft part two", "reviewer", None]
  assert orchestrator.seen["survey the field"] == []
  assert orchestrator.seen["draft part one"] == [
    "blackboard:criteria=primary sources only",
    'blackboard:outline={"sections": 3}',
  ]
  assert orchestrator.seen["review"] == ["blackboard:criteria=primary sources only"]
  assert run.progress.agents_completed == 4
  assert [p.status for p in run.progress.phases] == ["completed"]


@pytest.mark.asyncio
async def test_a_program_past_its_agent_limit_fails_before_spawning():
  async def program(ctx):
    await ctx.agent("one")
    await ctx.agent("two")

  with pytest.raises(DynamicWorkflowLimitError):
    await _runner(_Orchestrator()).run_dynamic_workflow(program, limits=DynamicWorkflowLimits(max_agents_per_run=1))
