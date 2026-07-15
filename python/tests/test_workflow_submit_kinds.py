import json

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    LoopResult,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
    WorkflowNodeSpec,
    WorkflowSpec,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy, Message
from deepstrike.runtime.runner import _parse_submit_workflow_nodes_args
from deepstrike.runtime.kernel_step import kernel_action
from deepstrike.types.agent import workflow_node_spec_to_kernel


# ── M2: the submit parser passes control-flow kinds through (no longer downgraded to spawn) ──


def test_submit_parser_passes_control_flow_kinds_through():
    args = json.dumps({
        "nodes": [
            {"task": "refine", "role": "implement", "loop": {"max_iters": 3}},
            {"task": "route", "role": "plan", "classify": {"branches": [{"label": "a", "nodes": [0]}]}},
            {"task": "pick", "role": "plan", "tournament": {"entrants": ["x", "y"]}},
            {"task": "merge", "role": "custom", "reducer": "concat"},
            {"task": "explore", "role": "explore", "model_hint": "haiku"},
        ]
    })
    nodes = _parse_submit_workflow_nodes_args(args)
    assert len(nodes) == 5
    assert nodes[0].loop == {"max_iters": 3}
    assert nodes[1].classify == {"branches": [{"label": "a", "nodes": [0]}]}
    assert nodes[2].tournament == {"entrants": ["x", "y"]}
    assert nodes[3].reducer == "concat"
    assert nodes[4].model_hint == "haiku"
    # …and each lowers to the right kernel NodeKind.
    assert workflow_node_spec_to_kernel(nodes[0])["kind"] == {"type": "loop", "max_iters": 3}
    assert workflow_node_spec_to_kernel(nodes[2])["kind"]["type"] == "tournament"
    assert "kind" not in workflow_node_spec_to_kernel(nodes[4])  # model_hint alone ⇒ plain spawn


# ── M2: an agent submits a tournament at runtime → kernel expands + judges it end-to-end ──


def _runner(orch):
    r = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    kernel_action(rt, [], {"kind": "start_run", "task": {"goal": "parent", "criteria": []}})
    r._active_kernel = rt
    r._current_session_id = "sess"
    return r


class _SubmitStub:
    """A coordinator node submits a tournament at runtime; judges report a winner."""

    def __init__(self) -> None:
        self.goals: list[str] = []

    async def run(self, ctx):
        goal = ctx.spec.goal
        self.goals.append(goal)
        submitted = []
        content = "ok"
        if "coordinate" in goal:
            submitted = [WorkflowNodeSpec(task="pick the best", role="plan", tournament={"entrants": ["x", "y"]})]
        elif "CANDIDATE left" in goal:  # a judge spawn
            content = json.dumps({"winner": "left"})
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(
                termination="completed",
                turns_used=1,
                total_tokens_used=1,
                final_message=Message(role="assistant", content=content),
            ),
            submitted_nodes=submitted,
        )


@pytest.mark.asyncio
async def test_agent_submitted_tournament_runs_end_to_end():
    orch = _SubmitStub()
    runner = _runner(orch)
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="coordinate the tournament", role="plan")])
    outcome = await runner.run_workflow(spec)
    assert "wf-node0" in outcome["completed"]  # coordinator completed
    # the submitted tournament expanded into entrants + a judge over the two candidates
    assert any("CANDIDATE left" in g for g in orch.goals), "a judge ran over the submitted tournament"
