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
    extract_loop_continue,
    extract_classify_branch,
    extract_judge_winner,
)
from deepstrike.types.agent import sub_agent_result_to_kernel
from deepstrike._kernel import KernelRuntime, LoopPolicy, Message, ToolCall
from deepstrike.runtime.kernel_step import kernel_action


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
    assert extract_loop_continue('{"loop_continue": false}') is False
    assert extract_loop_continue('done: {"done": true}') is False
    assert extract_loop_continue("no json") is None

    assert extract_classify_branch('{"branch": "bug"}', ["bug", "feature"]) == "bug"
    assert extract_classify_branch("feature", ["bug", "feature"]) == "feature"
    assert extract_classify_branch("garbage", ["bug", "feature"]) is None

    assert extract_judge_winner('{"winner": "right"}') == "right"
    assert extract_judge_winner("totally unparseable") == "left"

    assert "4" in loop_instruction(4)
    assert '"bug"' in classify_instruction(["bug", "feature"])
    assert "LEFTOUT" in judge_goal("criterion", "LEFTOUT", "RIGHTOUT")


# ── End-to-end drive through the real kernel + runner + a content-aware stub ──


class _ControlFlowStub:
    """Returns content the runner's extractors parse into control-flow signals, keyed off the goal's
    injected instruction (loop / classify / judge), so a full run_workflow drive exercises the path."""

    def __init__(self, *, classify_pick="a", loop_stop=True, judge_pick="left") -> None:
        self.classify_pick = classify_pick
        self.loop_stop = loop_stop
        self.judge_pick = judge_pick
        self.goals: list[str] = []

    async def run(self, ctx) -> SubAgentResult:
        goal = ctx.spec.goal
        self.goals.append(goal)
        if "CANDIDATE left" in goal:
            content = json.dumps({"winner": self.judge_pick})
        elif "Classify the input" in goal:
            content = json.dumps({"branch": self.classify_pick})
        elif "runs as a LOOP" in goal:
            content = json.dumps({"loop_continue": not self.loop_stop})
        else:
            content = f"candidate::{ctx.spec.identity.agent_id}"
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(
                termination="completed",
                turns_used=1,
                total_tokens_used=1,
                final_message=Message(role="assistant", content=content),
            ),
        )


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


@pytest.mark.asyncio
async def test_loop_node_stops_early_via_signal():
    orch = _ControlFlowStub(loop_stop=True)
    runner = _runner(orch)
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="refine", role="implement", loop={"max_iters": 5}),
        WorkflowNodeSpec(task="ship", role="implement", depends_on=[0]),
    ])
    outcome = await runner.run_workflow(spec)
    assert "wf-node0" in outcome["completed"] and "wf-node1" in outcome["completed"]
    # The loop ran exactly once (stopped early) — only one iteration goal was dispatched.
    assert sum(1 for g in orch.goals if "runs as a LOOP" in g) == 1


@pytest.mark.asyncio
async def test_classify_node_routes_and_prunes():
    orch = _ControlFlowStub(classify_pick="a")
    runner = _runner(orch)
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="route", role="plan",
                         classify={"branches": [{"label": "a", "nodes": [1]}, {"label": "b", "nodes": [2]}]}),
        WorkflowNodeSpec(task="branch-a", role="implement", depends_on=[0]),
        WorkflowNodeSpec(task="branch-b", role="implement", depends_on=[0]),
    ])
    outcome = await runner.run_workflow(spec)
    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1"]
    assert outcome["failed"] == ["wf-node2"]


@pytest.mark.asyncio
async def test_tournament_node_picks_winner_and_promotes_dependent():
    orch = _ControlFlowStub(judge_pick="left")
    runner = _runner(orch)
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="pick the best", role="plan", tournament={"entrants": ["x", "y"]}),
        WorkflowNodeSpec(task="use winner", role="implement", depends_on=[0]),
    ])
    outcome = await runner.run_workflow(spec)
    # Controller (node0) + dependent (node1) both complete; a judge ran over the two candidates.
    assert "wf-node0" in outcome["completed"] and "wf-node1" in outcome["completed"]
    assert any("CANDIDATE left" in g for g in orch.goals)
