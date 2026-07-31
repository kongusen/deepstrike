import json

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    WorkflowNodeSpec,
    WorkflowSpec,
)
from deepstrike.runtime.runner import _parse_submit_workflow_nodes_args
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
