"""Sub-agent harness integration — mirrors Node subAgentHarness path."""
from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from deepstrike.harness.harness import AttemptOutcome, Verdict
from deepstrike.runtime.runner import RuntimeOptions, SubAgentHarnessConfig
from deepstrike.runtime.sub_agent_orchestrator import SubAgentOrchestrator, SubAgentRunContext
from deepstrike.types.agent import (
  AgentIdentity,
  AgentRunSpec,
  AgentProcessChangedObservation,
  MilestoneContract,
  MilestonePhase,
)


def _ctx(*, with_harness: bool) -> SubAgentRunContext:
  spec = AgentRunSpec(
    identity=AgentIdentity(agent_id="child-1", session_id="child-session"),
    role="implement",
    goal="Write hello world",
    milestones=MilestoneContract(phases=[
      MilestonePhase(id="p1", criteria=["Output contains hello"]),
    ]),
  )
  manifest = AgentProcessChangedObservation(
    agent_id="child-1",
    parent_session_id="parent-session",
    role="implement",
    isolation="shared",
    context_inheritance="none",
    permitted_capability_ids=["tool:add"],
  )
  parent_opts = RuntimeOptions(
    provider=MagicMock(),
    session_log=MagicMock(),
    sub_agent_harness=SubAgentHarnessConfig(
      eval_provider=MagicMock(),
      max_attempts=2,
    ) if with_harness else None,
  )
  return SubAgentRunContext(
    parent_opts=parent_opts,
    parent_session_id="parent-session",
    spec=spec,
    manifest=manifest,
    session_log=parent_opts.session_log,
    harness=parent_opts.sub_agent_harness,
  )


@pytest.mark.asyncio
async def test_harness_path_uses_attempt_loop_and_preserves_two_axes():
  ctx = _ctx(with_harness=True)
  orchestrator = SubAgentOrchestrator()

  mock_loop = MagicMock()
  mock_loop.run = AsyncMock(return_value=AttemptOutcome(
    outcome="passed",
    run_status="completed",
    result="hello world",
    attempts=2,
    turns=2,
    total_tokens=42,
    verdict=Verdict(passed=True, overall_score=1, feedback="ok"),
  ))

  with patch("deepstrike.harness.harness.AttemptLoop", return_value=mock_loop):
    with patch("deepstrike.runtime.sub_agent_orchestrator.RuntimeRunner") as runner_cls:
      runner_cls.return_value = MagicMock()
      result = await orchestrator.run(ctx)

  mock_loop.run.assert_awaited_once()
  req = mock_loop.run.await_args.args[0]
  assert req.goal == "Write hello world"
  assert [c.text for c in req.criteria or []] == ["Output contains hello"]
  assert result.result.termination == "completed"
  assert result.result.turns_used == 2
  assert result.result.total_tokens_used == 42
  assert result.result.attempt["outcome"] == "passed"
  assert result.result.attempt["verdict"].passed is True


@pytest.mark.asyncio
async def test_direct_path_skips_attempt_loop():
  ctx = _ctx(with_harness=False)
  orchestrator = SubAgentOrchestrator()

  mock_runner = MagicMock()

  async def _run(**kwargs):
    from deepstrike.providers.stream import DoneEvent
    yield DoneEvent(iterations=1, total_tokens=10, status="completed")

  mock_runner.run = _run

  with patch("deepstrike.harness.harness.AttemptLoop") as harness_cls:
    with patch("deepstrike.runtime.sub_agent_orchestrator.RuntimeRunner", return_value=mock_runner):
      result = await orchestrator.run(ctx)

  harness_cls.assert_not_called()
  assert result.result.termination == "completed"
