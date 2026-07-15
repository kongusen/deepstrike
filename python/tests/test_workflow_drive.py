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
from deepstrike.runtime.kernel_step import kernel_action, kernel_apply, kernel_maybe_action
from deepstrike.runtime.session_repair import RecoveredNodeOutcome


def _start_runtime(runtime: KernelRuntime) -> None:
    kernel_action(runtime, [], {"kind": "start_run", "task": {"goal": "parent", "criteria": []}})


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
        "dep_policy": "all_success",
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
async def test_standalone_workflow_charges_node_count_to_group():
    """Gap-a: a standalone (bootstrapped) run_workflow under a RunGroup counts its nodes toward the
    cumulative spawn axis. Nodes are member runs whose own charge is 0 spawns; the envelope kernel's
    TaskTable holds one proc per node, so its local_subagents_spawned() is exactly the node count."""
    from deepstrike import RunGroup, InMemoryGroupBudgetStore

    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="wf-group", budget_store=store)
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
        run_group=group,
    ))
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
    ])
    outcome = await runner.run_workflow(spec)

    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
    ledger = await store.read("wf-group")
    assert ledger.subagents_spawned >= 2  # gap-a: the 2 nodes are counted as cumulative spawns
    assert len(await store.members("wf-group")) >= 1  # standalone workflow session joined (lineage)


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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])
    outcome = await runner.run_workflow(spec)

    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
    # Workers ran first (parallel), then synth — all goals were dispatched.
    assert sorted(orch.goals) == ["synth", "w0", "w1"]
    assert orch.goals[-1] == "synth"  # synth only after both workers
from deepstrike.runtime.session_repair import (
    build_workflow_node_completed_event,
    recover_workflow_node_outcomes,
)


def test_build_workflow_node_completed_event_shape():
    event = build_workflow_node_completed_event(
        turn=5,
        agent_id="wf-node3",
        status="completed",
        termination="completed",
    )
    assert event["kind"] == "workflow_node_completed"
    assert event["turn"] == 5
    assert event["agent_id"] == "wf-node3"
    assert event["termination"] == "completed"


def test_recover_workflow_node_outcomes_extracts_completed():
    from deepstrike.runtime.session_log import SessionEntry

    events = [
        SessionEntry(seq=0, event={"kind": "run_started", "run_id": "s1", "goal": "test", "criteria": []}),
        SessionEntry(seq=1, event=build_workflow_node_completed_event(
            turn=1, agent_id="wf-node0", status="completed", termination="completed", classify_branch="a",
        )),
        SessionEntry(seq=2, event=build_workflow_node_completed_event(turn=2, agent_id="wf-node1", status="failed", termination="error")),
        SessionEntry(seq=3, event=build_workflow_node_completed_event(turn=3, agent_id="wf-node2", status="completed_partial", termination="timeout")),
        SessionEntry(seq=4, event={"kind": "run_terminal", "reason": "done", "turns_used": 3, "total_tokens": 10}),
    ]
    completed = recover_workflow_node_outcomes(events)
    # W-1: records (not bare ids) — signals + output ride along for faithful control-flow replay.
    assert [r.agent_id for r in completed] == ["wf-node0", "wf-node1", "wf-node2"]
    assert completed[0].classify_branch == "a"
    assert completed[0].status == "completed"
    assert completed[1].status == "failed"
    assert completed[2].status == "completed_partial"


def test_recover_workflow_node_outcomes_empty_stream():
    assert recover_workflow_node_outcomes([]) == []
    assert recover_workflow_node_outcomes([
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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    # Resume with node0 already completed.
    outcome = await runner.run_workflow(spec, resumed_outcomes=[RecoveredNodeOutcome(
        agent_id="wf-node0", status="completed", termination="completed",
    )])
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0]),
    ])

    # Both nodes already completed → kernel skips dispatch, batch is empty.
    outcome = await runner.run_workflow(spec, resumed_outcomes=[
        RecoveredNodeOutcome(agent_id=node_id, status="completed", termination="completed")
        for node_id in ("wf-node0", "wf-node1")
    ])
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    # Seed the session log with completed nodes.
    await runner._opts.session_log.append("sess", build_workflow_node_completed_event(
        turn=1, agent_id="wf-node0", status="completed", termination="completed",
    ))
    await runner._opts.session_log.append("sess", build_workflow_node_completed_event(
        turn=2, agent_id="wf-node1", status="failed", termination="error",
    ))

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    # resume_workflow reads the log and extracts completed nodes.
    outcome = await runner.resume_workflow(spec)
    # Only node0 was recovered as completed, so it's skipped.
    assert "wf-node0" in [n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]
    # all_success is fail-closed: the recovered failed dependency blocks synthesis.
    assert "wf-node2" in [
        n.node_id for n in outcome.node_outcomes if n.status == "skipped_upstream_failed"
    ]
    assert "wf-node1" in [n.node_id for n in outcome.node_outcomes if n.status == "failed"]


def test_submit_workflow_nodes_to_kernel_shape():
    from deepstrike import submit_workflow_nodes_to_kernel

    event = submit_workflow_nodes_to_kernel([WorkflowNodeSpec(task="more", role="implement")])
    assert event == {
        "kind": "submit_workflow_nodes",
        "nodes": [
            {"task": {"goal": "more", "criteria": []}, "role": "implement",
             "isolation": "shared", "context_inheritance": "none",
             "dep_policy": "all_success"},
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


def test_submit_workflow_nodes_stamps_submitter_only_when_provided():
    from deepstrike import submit_workflow_nodes_to_kernel

    plain = submit_workflow_nodes_to_kernel([WorkflowNodeSpec(task="x", role="implement")])
    assert "submitter_agent_id" not in plain
    stamped = submit_workflow_nodes_to_kernel(
        [WorkflowNodeSpec(task="x", role="implement")], "wf-node0"
    )
    assert stamped["submitter_agent_id"] == "wf-node0"


def test_g1_quarantined_submitter_cannot_escalate_over_abi():
    # G1: a quarantined submitter's node is coerced to quarantined in-kernel; the spawn-time gate
    # then denies its (default, write-capable) isolation — so the escalated node never spawns.
    from deepstrike import submit_workflow_nodes_to_kernel

    rt = KernelRuntime(LoopPolicy(max_tokens=128000))
    _start_runtime(rt)
    initial = kernel_maybe_action(rt, [], {
        "kind": "load_workflow",
        "spec": {"nodes": [{
            "task": {"goal": "read-untrusted", "criteria": []},
            "role": "explore",
            "isolation": "read_only",
            "context_inheritance": "none",
            "trust": "quarantined",
        }]},
        "parent_session_id": "sess",
    })
    assert initial is not None and initial.kind == "spawn_workflow"
    kernel_apply(rt, [], {
        "kind": "workflow_spawn_result", "effect_id": initial.effect_id,
        "started_agent_ids": [n["agent_id"] for n in initial.nodes or []], "failures": [],
    })

    escalated = kernel_maybe_action(rt, [], submit_workflow_nodes_to_kernel(
        [WorkflowNodeSpec(task="act-with-privilege", role="implement")], "wf-node0"
    ))
    spawned = [n["agent_id"] for n in (escalated.nodes or [])] if escalated else []
    assert "wf-node1" not in spawned, "quarantined submitter's write-capable node must be denied"

    # Control: no submitter id → no coercion → the same node spawns.
    rt2 = KernelRuntime(LoopPolicy(max_tokens=128000))
    _start_runtime(rt2)
    initial2 = kernel_maybe_action(rt2, [], {
        "kind": "load_workflow",
        "spec": {"nodes": [{
            "task": {"goal": "root", "criteria": []},
            "role": "implement",
            "isolation": "shared",
            "context_inheritance": "none",
        }]},
        "parent_session_id": "sess",
    })
    assert initial2 is not None and initial2.kind == "spawn_workflow"
    kernel_apply(rt2, [], {
        "kind": "workflow_spawn_result", "effect_id": initial2.effect_id,
        "started_agent_ids": [n["agent_id"] for n in initial2.nodes or []], "failures": [],
    })
    ok = kernel_maybe_action(rt2, [], submit_workflow_nodes_to_kernel(
        [WorkflowNodeSpec(task="act-with-privilege", role="implement")]
    ))
    spawned_ok = [n["agent_id"] for n in (ok.nodes or [])] if ok else []
    assert "wf-node1" in spawned_ok


def test_recover_submitted_workflow_nodes_in_order():
    from deepstrike.runtime.session_repair import (
        build_workflow_nodes_submitted_event,
        recover_submitted_workflow_nodes,
    )

    e1 = build_workflow_nodes_submitted_event(turn=1, nodes=[{"task": {"goal": "a"}}], base_index=1)
    e2 = build_workflow_nodes_submitted_event(turn=2, nodes=[{"task": {"goal": "b"}}], base_index=2)
    submissions, bases, submitters = recover_submitted_workflow_nodes([e1, e2])
    assert submissions == [
        [{"task": {"goal": "a"}}],
        [{"task": {"goal": "b"}}],
    ]
    assert bases == [1, 2]
    assert submitters == [None, None]

    # Recorded bases come back parallel; a missing base rejects recovery.
    # W-N3: submitters come back parallel too (None = host/bootstrap submission).
    b1 = build_workflow_nodes_submitted_event(
        turn=1, nodes=[{"task": {"goal": "a"}}], base_index=3, submitter_agent_id="wf-node0",
    )
    b2 = build_workflow_nodes_submitted_event(turn=2, nodes=[{"task": {"goal": "b"}}], base_index=5)
    _, bases_full, submitters_full = recover_submitted_workflow_nodes([b1, b2])
    assert bases_full == [3, 5]
    assert submitters_full == ["wf-node0", None]
    invalid = {"kind": "workflow_nodes_submitted", "turn": 3, "nodes": []}
    with pytest.raises(ValueError, match="missing required base_index"):
        recover_submitted_workflow_nodes([b1, invalid])


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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")])
    batch = submit_workflow_nodes_to_kernel([WorkflowNodeSpec(task="discovered", role="implement")])["nodes"]

    # Root recovered as completed; one submission re-applied → wf-node1 reconstructed and run.
    outcome = await runner.run_workflow(
        spec,
        resumed_outcomes=[RecoveredNodeOutcome(
            agent_id="wf-node0", status="completed", termination="completed",
        )],
        resumed_submissions=[batch],
        resumed_submission_bases=[1],
    )
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
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
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")])
    outcome = await runner.run_workflow(spec)

    # Both the root and the dynamically-submitted node completed.
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
    assert "discovered" in orch.goals


# ── G3 structured output ─────────────────────────────────────────────────────────────────────────

def test_g3_validate_against_schema_subset():
    from deepstrike.runtime.output_schema import validate_against_schema, extract_json_value

    schema = {
        "type": "object",
        "required": ["verdict", "score"],
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "fail"]},
            "score": {"type": "integer"},
            "notes": {"type": "array", "items": {"type": "string"}},
        },
    }
    assert validate_against_schema({"verdict": "pass", "score": 3, "notes": ["ok"]}, schema) == []
    assert validate_against_schema({"verdict": "pass"}, schema)  # missing required
    assert validate_against_schema({"verdict": "pass", "score": 1.5}, schema)  # non-integer
    assert validate_against_schema({"verdict": "maybe", "score": 1}, schema)  # out of enum
    assert validate_against_schema("nope", schema)  # wrong type
    # bool must not satisfy integer
    assert validate_against_schema({"verdict": "pass", "score": True}, schema)

    assert extract_json_value('{"a":1}') == {"a": 1}
    assert extract_json_value("```json\n{\"a\":1}\n```") == {"a": 1}
    assert extract_json_value("result: {\"a\":1}.") == {"a": 1}
    assert extract_json_value("no json") is None


_G3_SCHEMA = {"type": "object", "required": ["verdict"], "properties": {"verdict": {"type": "string"}}}


def _g3_runner(orch, *, attempts=2):
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        workflow_schema_validation_attempts=attempts,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=1000))
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"
    return runner


@pytest.mark.asyncio
async def test_g3_run_workflow_accepts_conforming_output_first_attempt():
    from deepstrike._kernel import Message

    class _Orch:
        def __init__(self):
            self.goals = []

        async def run(self, ctx):
            self.goals.append(ctx.spec.goal)
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content='{"verdict":"pass"}')),
            )

    orch = _Orch()
    runner = _g3_runner(orch)
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="judge", role="verify", output_schema=_G3_SCHEMA)])
    outcome = await runner.run_workflow(spec)
    assert [n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")] == ["wf-node0"]
    assert len(orch.goals) == 1
    assert "JSON Schema" in orch.goals[0]


@pytest.mark.asyncio
async def test_g3_run_workflow_retries_once_then_accepts():
    from deepstrike._kernel import Message

    class _Orch:
        def __init__(self):
            self.calls = 0
            self.goals = []

        async def run(self, ctx):
            self.calls += 1
            self.goals.append(ctx.spec.goal)
            content = "I think it passes." if self.calls == 1 else '{"verdict":"pass"}'
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content=content)),
            )

    orch = _Orch()
    runner = _g3_runner(orch)
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="judge", role="verify", output_schema=_G3_SCHEMA)])
    outcome = await runner.run_workflow(spec)
    assert orch.calls == 2
    assert "did NOT conform" in orch.goals[1]
    assert [n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")] == ["wf-node0"]


@pytest.mark.asyncio
async def test_g3_run_workflow_fails_node_when_never_conforms():
    from deepstrike._kernel import Message

    class _Orch:
        def __init__(self):
            self.calls = 0

        async def run(self, ctx):
            self.calls += 1
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content="never valid")),
            )

    orch = _Orch()
    runner = _g3_runner(orch)
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="judge", role="verify", output_schema=_G3_SCHEMA)])
    outcome = await runner.run_workflow(spec)
    assert orch.calls == 2
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == ["wf-node0"]


@pytest.mark.asyncio
async def test_g3_run_workflow_uses_configured_attempt_bound():
    from deepstrike._kernel import Message

    class _Orch:
        def __init__(self):
            self.calls = 0

        async def run(self, ctx):
            self.calls += 1
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content="never valid")),
            )

    orch = _Orch()
    runner = _g3_runner(orch, attempts=3)
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="judge", role="verify", output_schema=_G3_SCHEMA)])
    outcome = await runner.run_workflow(spec)
    assert orch.calls == 3
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == ["wf-node0"]


def test_g3_rejects_unsafe_attempt_bound():
    with pytest.raises(ValueError, match="between 1 and 16"):
        _g3_runner(object(), attempts=0)


# ── G4 budget-as-signal ──────────────────────────────────────────────────────────────────────────

def test_g4_workflow_budget_note_formats_and_omits():
    from deepstrike import workflow_budget_note

    full = {
        "nodes_used": 1, "nodes_max": 5, "nodes_remaining": 4,
        "running_subagents": 1, "max_concurrent_subagents": 3, "concurrency_remaining": 2,
        "tokens_used": 2500, "tokens_max": 10000, "tokens_remaining": 7500,
    }
    note = workflow_budget_note(full)
    assert "nodes 1/5 used, 4 remaining" in note
    assert "concurrency 1/3 running, 2 free" in note
    # M4/G5: token headroom surfaced so a coordinator can scale to "use N tokens".
    assert "tokens 2500/10000 used, 7500 remaining" in note
    assert workflow_budget_note(None) == ""
    assert workflow_budget_note({"nodes_used": 2, "running_subagents": 1}) == ""


@pytest.mark.asyncio
async def test_g4_run_workflow_surfaces_budget_into_node_goal():
    from deepstrike._kernel import Message

    class _Orch:
        def __init__(self):
            self.goals = []

        async def run(self, ctx):
            self.goals.append(ctx.spec.goal)
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content="ok")),
            )

    orch = _Orch()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=128000))
    _start_runtime(rt)
    kernel_apply(rt, [], {"kind": "set_resource_quota",
            "quota": {"max_workflow_nodes": 5, "max_concurrent_subagents": 3}})
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="coordinate", role="implement")])
    await runner.run_workflow(spec)
    assert len(orch.goals) == 1
    assert "[workflow budget]" in orch.goals[0]
    assert "nodes 1/5 used, 4 remaining" in orch.goals[0]


# ── G2 deterministic compute (reduce nodes) ──────────────────────────────────────────────────────

def test_g2_builtin_reducers():
    from deepstrike import builtin_reducers

    assert builtin_reducers["dedupe_lines"]([
        {"agent_id": "a", "output": "x\ny\nx"},
        {"agent_id": "b", "output": "y\nz"},
    ]) == "x\ny\nz"
    merged = builtin_reducers["merge_json_arrays"]([
        {"agent_id": "a", "output": '[{"id":1},{"id":2}]'},
        {"agent_id": "b", "output": '[{"id":2},{"id":3}]'},
    ])
    assert json.loads(merged) == [{"id": 1}, {"id": 2}, {"id": 3}]
    assert builtin_reducers["count"]([
        {"agent_id": "a", "output": "x"}, {"agent_id": "b", "output": "  "},
    ]) == "1"


def test_g2_reducer_lowers_to_kernel_node_kind():
    from deepstrike import workflow_node_spec_to_kernel

    k = workflow_node_spec_to_kernel(WorkflowNodeSpec(task="merge", role="implement", reducer="dedupe_lines", depends_on=[0, 1]))
    assert k["kind"] == {"type": "reduce", "reducer": "dedupe_lines"}
    assert k["depends_on"] == [0, 1]


@pytest.mark.asyncio
async def test_g2_run_workflow_runs_reduce_node_without_llm():
    from deepstrike._kernel import Message

    agent_calls = {"n": 0}

    class _Orch:
        async def run(self, ctx):
            agent_calls["n"] += 1
            content = "alpha\nshared" if ctx.spec.identity.agent_id == "wf-node0" else "shared\nbeta"
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content=content)),
            )

    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_Orch(),
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=128000))
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="worker A", role="explore"),
        WorkflowNodeSpec(task="worker B", role="explore"),
        WorkflowNodeSpec(task="merge", role="implement", reducer="dedupe_lines", depends_on=[0, 1]),
    ])
    outcome = await runner.run_workflow(spec)
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert agent_calls["n"] == 2  # only the two workers called an agent; the reduce ran in-process


@pytest.mark.asyncio
async def test_g2_unknown_reducer_fails_node():
    from deepstrike._kernel import Message

    class _Orch:
        async def run(self, ctx):
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1,
                                  final_message=Message(role="assistant", content="x")),
            )

    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_Orch(),
        max_tokens=1000,
    ))
    rt = KernelRuntime(LoopPolicy(max_tokens=128000))
    _start_runtime(rt)
    runner._active_kernel = rt
    runner._current_session_id = "sess"

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="worker", role="explore"),
        WorkflowNodeSpec(task="merge", role="implement", reducer="nope", depends_on=[0]),
    ])
    outcome = await runner.run_workflow(spec)
    assert "wf-node1" in [n.node_id for n in outcome.node_outcomes if n.status == "failed"]


@pytest.mark.asyncio
async def test_run_workflow_bootstraps_standalone():
    """No active run: run_workflow auto-bootstraps a kernel, drives the DAG, then tears it down."""
    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    # Called on a bare runner — no _active_kernel hack.
    outcome = await runner.run_workflow(spec)
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []

    # Bootstrapped kernel was torn down → runner is reusable.
    assert runner._active_kernel is None
    assert runner._current_session_id is None
    second = await runner.run_workflow(spec)
    assert sorted([
        n.node_id for n in second.node_outcomes
        if n.status in ("completed", "completed_partial")
    ]) == ["wf-node0", "wf-node1", "wf-node2"]


@pytest.mark.asyncio
async def test_standalone_workflow_requires_durable_start_before_dispatch():
    class _FailingStartLog(InMemorySessionLog):
        async def append(self, session_id, event):
            if event["kind"] == "run_started":
                raise RuntimeError("session log unavailable")
            return await super().append(session_id, event)

    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=_FailingStartLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="w0", role="explore")])

    with pytest.raises(RuntimeError, match="session log unavailable"):
        await runner.run_workflow(spec, session_id="durable-start")
    assert orch.goals == []
    assert runner._active_kernel is None
    assert runner._current_session_id is None


@pytest.mark.asyncio
async def test_resume_workflow_standalone_by_session_id():
    """Standalone resume reads the prior session by id; completed nodes are not re-run."""
    orch = _StubOrchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="w0", role="explore"),
        WorkflowNodeSpec(task="w1", role="explore"),
        WorkflowNodeSpec(task="synth", role="plan", depends_on=[0, 1]),
    ])

    await runner.run_workflow(spec, session_id="resume-me")
    assert len(orch.goals) == 3

    resumed = await runner.resume_workflow(spec, session_id="resume-me")
    assert sorted([n.node_id for n in resumed.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    # No new dispatches — every node was recovered as already complete.
    assert len(orch.goals) == 3


@pytest.mark.asyncio
async def test_resume_workflow_requires_session():
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))
    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="w0", role="explore")])
    with pytest.raises(RuntimeError, match="active parent run or an explicit session_id"):
        await runner.resume_workflow(spec)
