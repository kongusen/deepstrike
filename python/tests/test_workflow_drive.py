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
    workflow_spec_to_kernel,
    fanout_synthesize,
    generate_and_filter,
    verify_rules,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy


class _StubProvider:
    async def stream(self, context, tools, extensions=None, state=None):  # pragma: no cover
        from deepstrike.providers.stream import TextDelta

        yield TextDelta(delta="x")


class _StubOrchestrator:
    """Records goals it was asked to run; reports each node as completed."""

    def __init__(self) -> None:
        self.goals: list[str] = []

    async def run(self, ctx) -> SubAgentResult:
        self.goals.append(ctx.spec.goal)
        return SubAgentResult(
            agent_id=ctx.spec.identity.agent_id,
            result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1),
        )


def test_workflow_spec_to_kernel_shape():
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore", isolation="read_only", context_inheritance="system_only"),
        WorkflowNodeSpec(task={"goal": "synth", "criteria": ["merge"]}, role="plan", depends_on=[0]),
    ])
    k = workflow_spec_to_kernel(spec)
    assert k["nodes"][0] == {
        "task": {"goal": "w0", "criteria": []},
        "role": "explore",
        "isolation": "read_only",
        "context_inheritance": "system_only",
    }
    assert k["nodes"][1]["task"] == {"goal": "synth", "criteria": ["merge"]}
    assert k["nodes"][1]["depends_on"] == [0]


def test_workflow_templates_shapes():
    fan = fanout_synthesize(["a", "b", "c"], "merge")
    assert len(fan.nodes) == 4
    assert fan.nodes[0].role == "explore" and fan.nodes[0].isolation == "read_only"
    assert fan.nodes[3].role == "plan" and fan.nodes[3].depends_on == [0, 1, 2]

    gen = generate_and_filter(["x", "y"], "dedupe")
    assert gen.nodes[0].role == "implement"
    assert gen.nodes[2].role == "verify" and gen.nodes[2].depends_on == [0, 1]

    ver = verify_rules(["r1", "r2"], "skeptic")
    assert len(ver.nodes) == 3
    for n in ver.nodes[:2]:
        assert n.role == "verify" and n.context_inheritance == "none" and n.depends_on == []
    assert ver.nodes[2].depends_on == [0, 1]
    assert len(verify_rules(["only"]).nodes) == 1


@pytest.mark.asyncio
async def test_run_workflow_drives_fanout_to_completion():
    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    # Establish an active parent run on a kernel (run_workflow runs mid-run).
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])
    outcome = await runner.run_workflow(spec)

    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert outcome["failed"] == []
    # Workers ran first (parallel), then synth — all goals were dispatched.
    assert sorted(orch.goals) == ["synth", "w0", "w1"]
    assert orch.goals[-1] == "synth"  # synth only after both workers
from deepstrike.runtime.session_repair import (
    build_workflow_node_completed_event,
    recover_completed_workflow_nodes,
)


def test_build_workflow_node_completed_event_shape():
    event = build_workflow_node_completed_event(
        turn=5,
        agent_id="wf-node3",
        termination="completed",
    )
    assert event["kind"] == "workflow_node_completed"
    assert event["turn"] == 5
    assert event["agent_id"] == "wf-node3"
    assert event["termination"] == "completed"


def test_recover_completed_workflow_nodes_extracts_completed():
    from deepstrike.runtime.session_log import SessionEntry

    events = [
        SessionEntry(seq=0, event={"kind": "run_started", "run_id": "s1", "goal": "test", "criteria": []}),
        SessionEntry(seq=1, event=build_workflow_node_completed_event(turn=1, agent_id="wf-node0", termination="completed")),
        SessionEntry(seq=2, event=build_workflow_node_completed_event(turn=2, agent_id="wf-node1", termination="failed")),
        SessionEntry(seq=3, event=build_workflow_node_completed_event(turn=3, agent_id="wf-node2", termination="completed")),
        SessionEntry(seq=4, event={"kind": "run_terminal", "reason": "done", "turns_used": 3, "total_tokens": 10}),
    ]
    completed = recover_completed_workflow_nodes(events)
    assert sorted(completed) == ["wf-node0", "wf-node2"]


def test_recover_completed_workflow_nodes_empty_stream():
    assert recover_completed_workflow_nodes([]) == []
    assert recover_completed_workflow_nodes([
        {"kind": "run_started", "run_id": "s1", "goal": "x", "criteria": []}
    ]) == []


@pytest.mark.asyncio
async def test_run_workflow_resumes_from_completed_nodes():
    from deepstrike import WorkflowSpec, WorkflowNodeSpec

    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    # Resume with node0 already completed.
    outcome = await runner.run_workflow(spec, resumed_completed=["wf-node0"])
    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert outcome["failed"] == []
    # Node0 is correctly skipped (not dispatched), only w1 and synth run.
    assert "w0" not in orch.goals
    assert "w1" in orch.goals
    assert "synth" in orch.goals


@pytest.mark.asyncio
async def test_run_workflow_with_all_nodes_resumed():
    from deepstrike import WorkflowSpec, WorkflowNodeSpec

    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0]),
    ])

    # Both nodes already completed → kernel skips dispatch, batch is empty.
    outcome = await runner.run_workflow(spec, resumed_completed=["wf-node0", "wf-node1"])
    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1"]
    assert outcome["failed"] == []
    # All nodes resumed → nothing dispatched.
    assert len(orch.goals) == 0


@pytest.mark.asyncio
async def test_resume_workflow_recovers_from_session_log():
    from deepstrike import WorkflowSpec, WorkflowNodeSpec

    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))

    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    # Seed the session log with completed nodes.
    await runner._opts.session_log.append("sess", build_workflow_node_completed_event(
        turn=1, agent_id="wf-node0", termination="completed",
    ))
    await runner._opts.session_log.append("sess", build_workflow_node_completed_event(
        turn=2, agent_id="wf-node1", termination="failed",
    ))

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    # resume_workflow reads the log and extracts completed nodes.
    outcome = await runner.resume_workflow(spec)
    # Only node0 was recovered as completed, so it's skipped.
    assert "wf-node0" in outcome["completed"]
    assert "wf-node2" in outcome["completed"]  # synth runs and completes
    # Node1 ran again (it was failed, not completed).
    assert outcome.get("failed") == []


def test_submit_workflow_nodes_to_kernel_shape():
    from deepstrike import submit_workflow_nodes_to_kernel

    event = submit_workflow_nodes_to_kernel([WorkflowNodeSpec(task="more", role="implement")])
    assert event == {
        "kind": "submit_workflow_nodes",
        "nodes": [
            {"task": {"goal": "more", "criteria": []}, "role": "implement",
             "isolation": "shared", "context_inheritance": "none"},
        ],
    }


def test_submit_workflow_nodes_carries_trust_and_deps():
    from deepstrike import submit_workflow_nodes_to_kernel

    event = submit_workflow_nodes_to_kernel([
        WorkflowNodeSpec(task="scrape", role="explore", isolation="read_only", trust="quarantined"),
        WorkflowNodeSpec(task="verify", role="verify", depends_on=[0]),
    ])
    assert event["nodes"][0]["trust"] == "quarantined"
    assert "trust" not in event["nodes"][1]  # default "trusted" omitted on the wire
    assert event["nodes"][1]["depends_on"] == [0]


def test_recover_submitted_workflow_nodes_in_order():
    from deepstrike.runtime.session_repair import (
        build_workflow_nodes_submitted_event,
        recover_submitted_workflow_nodes,
    )

    e1 = build_workflow_nodes_submitted_event(turn=1, nodes=[{"task": {"goal": "a"}}])
    e2 = build_workflow_nodes_submitted_event(turn=2, nodes=[{"task": {"goal": "b"}}])
    assert recover_submitted_workflow_nodes([e1, e2]) == [
        [{"task": {"goal": "a"}}],
        [{"task": {"goal": "b"}}],
    ]


@pytest.mark.asyncio
async def test_run_workflow_resumes_dynamically_appended_nodes():
    # R3-1: a workflow that dynamically appended a node is resumed via resumed_submissions; the kernel
    # re-applies the recorded submission so the appended node is reconstructed and runs.
    from deepstrike import submit_workflow_nodes_to_kernel

    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")])
    batch = submit_workflow_nodes_to_kernel([WorkflowNodeSpec(task="discovered", role="implement")])["nodes"]

    # Root recovered as completed; one submission re-applied → wf-node1 reconstructed and run.
    outcome = await runner.run_workflow(spec, resumed_completed=["wf-node0"], resumed_submissions=[batch])
    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1"]
    assert "discovered" in orch.goals


@pytest.mark.asyncio
async def test_run_workflow_submit_nodes_appends_and_completes():
    # R3-1: a node "submits" more work (via SubAgentResult.submitted_nodes); run_workflow sends
    # submit_workflow_nodes to the parent kernel BEFORE the node's completion, the appended node
    # spawns and runs, and the workflow finishes only after it too completes.
    class _SubmitOnceOrchestrator:
        def __init__(self):
            self.goals: list[str] = []
            self._submitted = False

        async def run(self, ctx):
            self.goals.append(ctx.spec.goal)
            submitted = []
            if not self._submitted and ctx.spec.goal == "root":
                self._submitted = True
                submitted = [WorkflowNodeSpec(task="discovered", role="implement")]
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1),
                submitted_nodes=submitted,
            )

    orch = _SubmitOnceOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    rt.step(json.dumps({"version": 1, "event": {"kind": "start_run", "task": {"goal": "parent", "criteria": []}}}))
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")])
    outcome = await runner.run_workflow(spec)

    # Both the root and the dynamically-submitted node completed.
    assert sorted(outcome["completed"]) == ["wf-node0", "wf-node1"]
    assert outcome["failed"] == []
    assert "discovered" in orch.goals
