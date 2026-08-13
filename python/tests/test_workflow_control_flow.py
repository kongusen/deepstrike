import json

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    SubAgentResult,
    LoopResult,
    WorkflowSpec,
    WorkflowNodeSpec,
    workflow_node_spec_to_kernel,
    loop_instruction,
    classify_instruction,
    judge_goal,
    extract_classify_branch,
    extract_judge_winner,
)
from deepstrike.types.agent import sub_agent_result_to_kernel
from deepstrike._kernel import Message, ToolCall


# ── Pure mapping ─────────────────────────────────────────────────────────────


def test_node_kind_mapping_for_control_flow():
    assert workflow_node_spec_to_kernel(
        WorkflowNodeSpec(task="refine", role="implement", loop={"max_iters": 3})
    )["kind"] == {"type": "loop", "max_iters": 3}

    assert workflow_node_spec_to_kernel(
        WorkflowNodeSpec(
            task="route",
            role="plan",
            classify={"branches": [{"label": "bug", "nodes": [1]}, {"label": "feature", "nodes": [2]}]},
        )
    )["kind"] == {"type": "classify", "branches": [{"label": "bug", "nodes": [1]}, {"label": "feature", "nodes": [2]}]}

    assert workflow_node_spec_to_kernel(
        WorkflowNodeSpec(task="pick", role="plan", tournament={"entrants": ["a", {"goal": "b", "criteria": ["x"]}]})
    )["kind"] == {"type": "tournament", "entrants": [{"goal": "a", "criteria": []}, {"goal": "b", "criteria": ["x"]}]}

    # plain spawn omits kind
    assert "kind" not in workflow_node_spec_to_kernel(WorkflowNodeSpec(task="do", role="implement"))


def test_node_kind_mutual_exclusion():
    with pytest.raises(ValueError, match="at most one"):
        workflow_node_spec_to_kernel(
            WorkflowNodeSpec(task="x", role="plan", loop={"max_iters": 2}, reducer="concat")
        )


def test_sub_agent_result_malformed_tool_args_does_not_brick():
    # A model wrote a truncated/garbled arguments string on its final turn; the OpenAIChat-family
    # non-streaming path passes it through verbatim. Serialization must degrade to {}, never raise.
    final = Message(role="assistant", content="", tool_calls=[ToolCall(id="t1", name="do", arguments='{"a": 1, "b')])
    res = SubAgentResult(agent_id="n0", result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1, final_message=final))
    out = sub_agent_result_to_kernel(res)  # must not raise
    assert out["result"]["final_message"]["tool_calls"][0]["arguments"] == {}

    # well-formed args still parse into an object
    final2 = Message(role="assistant", content="", tool_calls=[ToolCall(id="t1", name="do", arguments='{"a":1}')])
    res2 = SubAgentResult(agent_id="n0", result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1, final_message=final2))
    out2 = sub_agent_result_to_kernel(res2)
    assert out2["result"]["final_message"]["tool_calls"][0]["arguments"] == {"a": 1}


def test_signal_plumbing_is_additive():
    base = SubAgentResult(agent_id="wf-node0", result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1))
    plain = sub_agent_result_to_kernel(base)["result"]
    assert "loop_continue" not in plain and "classify_branch" not in plain and "tournament_winner" not in plain

    base.result.loop_continue = False
    base.result.classify_branch = "bug"
    base.result.tournament_winner = "wf-node2"
    res = sub_agent_result_to_kernel(base)["result"]
    assert res["loop_continue"] is False
    assert res["classify_branch"] == "bug"
    assert res["tournament_winner"] == "wf-node2"


# ── Extractors ───────────────────────────────────────────────────────────────


def test_extractors():
    assert extract_classify_branch('{"branch": "bug"}', ["bug", "feature"]) == "bug"
    assert extract_classify_branch("feature", ["bug", "feature"]) == "feature"
    assert extract_classify_branch("garbage", ["bug", "feature"]) is None

    assert extract_judge_winner('{"winner": "right"}') == "right"
    assert extract_judge_winner("totally unparseable") == "left"

    assert "4" in loop_instruction(4)
    assert '"bug"' in classify_instruction(["bug", "feature"])
    assert "LEFTOUT" in judge_goal("criterion", "LEFTOUT", "RIGHTOUT")


# ── Canonical host fail-closed on advanced WorkflowNode kinds ─────────────────


def _standalone_runner():
    return RuntimeRunner(RuntimeOptions(
        provider=None,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        max_tokens=1000,
    ))


@pytest.mark.asyncio
async def test_loop_node_fails_closed_on_canonical_kind():
    runner = _standalone_runner()
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="refine", role="implement", loop={"max_iters": 5}),
        WorkflowNodeSpec(task="ship", role="implement", depends_on=[0]),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason


@pytest.mark.asyncio
async def test_classify_node_fails_closed_on_canonical_kind():
    runner = _standalone_runner()
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="route", role="plan",
                         classify={"branches": [{"label": "a", "nodes": [1]}, {"label": "b", "nodes": [2]}]}),
        WorkflowNodeSpec(task="branch-a", role="implement", depends_on=[0]),
        WorkflowNodeSpec(task="branch-b", role="implement", depends_on=[0]),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason


@pytest.mark.asyncio
async def test_tournament_node_fails_closed_on_canonical_kind():
    runner = _standalone_runner()
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="pick the best", role="plan", tournament={"entrants": ["x", "y"]}),
        WorkflowNodeSpec(task="use winner", role="implement", depends_on=[0]),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason
