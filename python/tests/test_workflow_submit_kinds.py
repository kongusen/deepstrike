import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    WorkflowNodeSpec,
    WorkflowSpec,
)
from deepstrike.types.agent import workflow_node_spec_to_kernel


# ── Canonical node construction preserves control-flow kinds ──


def test_canonical_node_builder_preserves_control_flow_kinds():
    nodes = [
        WorkflowNodeSpec(task="refine", role="implement", loop={"max_iters": 3}),
        WorkflowNodeSpec(
            task="route",
            role="plan",
            classify={"branches": [{"label": "a", "nodes": [0]}]},
        ),
        WorkflowNodeSpec(task="pick", role="plan", tournament={"entrants": ["x", "y"]}),
        WorkflowNodeSpec(task="merge", role="custom", reducer="concat"),
        WorkflowNodeSpec(task="explore", role="explore", model_hint="haiku"),
    ]
    assert workflow_node_spec_to_kernel(nodes[0])["kind"] == {"type": "loop", "max_iters": 3}
    assert workflow_node_spec_to_kernel(nodes[2])["kind"]["type"] == "tournament"
    assert "kind" not in workflow_node_spec_to_kernel(nodes[4])  # model_hint alone ⇒ plain spawn


# ── Canonical host fail-closed on tournament nodes at load ───────────────────


@pytest.mark.asyncio
async def test_agent_submitted_tournament_fails_closed_at_load():
    runner = RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=1000,
    ))
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="pick the best", role="plan", tournament={"entrants": ["x", "y"]}),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason
