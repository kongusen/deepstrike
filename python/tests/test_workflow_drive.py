import json
import re

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
    # Standalone path — no activeKernel hack (mirrors Node workflow-standalone).
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
    outcome = await runner.run_workflow(spec)

    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1", "wf-node2"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
    # Workers ran first (parallel), then synth — all goals were dispatched.
    assert sorted(orch.goals) == ["synth", "w0", "w1"]
    assert orch.goals[-1] == "synth"  # synth only after both workers
    assert runner._active_kernel is None


from deepstrike.runtime.session_repair import build_workflow_node_completed_event


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


@pytest.mark.asyncio
async def test_g1_quarantined_workflow_fails_closed_until_canonical_trust():
    """Canonical WorkflowNode has no trust field yet — quarantined nodes fail closed at load."""
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(
            task="read-untrusted", role="explore", isolation="read_only", trust="quarantined",
        ),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: trust" in outcome.rejection.reason


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
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        max_tokens=1000,
    ))

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")])
    outcome = await runner.run_workflow(spec, session_id="wf-submit")

    # Both the root and the dynamically-submitted node completed.
    assert sorted([n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")]) == ["wf-node0", "wf-node1"]
    assert [n.node_id for n in outcome.node_outcomes if n.status == "failed"] == []
    assert "discovered" in orch.goals


@pytest.mark.asyncio
async def test_run_workflow_rejected_submission_keeps_child_completion():
    """ABI v3: parent-request admission is independent of ChildCompleted."""
    class _SubmitOnceOrchestrator:
        def __init__(self):
            self.goals: list[str] = []

        async def run(self, ctx):
            self.goals.append(ctx.spec.goal)
            return SubAgentResult(
                agent_id=ctx.spec.identity.agent_id,
                result=LoopResult(termination="completed", turns_used=1, total_tokens_used=1),
                submitted_nodes=[WorkflowNodeSpec(task="discovered", role="implement")],
            )

    orch = _SubmitOnceOrchestrator()
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        resource_quota={"max_workflow_nodes": 1},
        max_tokens=1000,
    ))

    outcome = await runner.run_workflow(
        WorkflowSpec(nodes=[WorkflowNodeSpec(task="root", role="implement")]),
        session_id="wf-parent-request-denied",
    )

    assert len(orch.goals) == 1
    assert [(node.node_id, node.status, node.termination) for node in outcome.node_outcomes] == [
        ("wf-node0", "completed", "completed")
    ]
    events = await session_log.read("wf-parent-request-denied")
    assert any(
        e.event.get("kind") == "kernel_observation"
        and e.event.get("observation_kind") == "control_request_rejected"
        and (e.event.get("raw") or {}).get("operation") == "submit_workflow_nodes"
        for e in events
    )


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
    return RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orch,
        workflow_schema_validation_attempts=attempts,
        max_tokens=1000,
    ))


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
        resource_quota={"max_workflow_nodes": 5, "max_concurrent_subagents": 3},
        max_tokens=1000,
    ))

    spec = WorkflowSpec(nodes=[WorkflowNodeSpec(task="coordinate", role="implement")])
    await runner.run_workflow(spec)
    assert len(orch.goals) == 1
    assert "[workflow budget]" in orch.goals[0]
    assert "concurrency capped at 3" in orch.goals[0]
    # Canonical v3 publishes the kernel-owned cap, not a host-authored remaining counter.
    import re
    assert re.search(r"tokens capped at \d+", orch.goals[0])


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
async def test_g2_run_workflow_fails_closed_on_reduce_node():
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="worker A", role="explore"),
        WorkflowNodeSpec(task="worker B", role="explore"),
        WorkflowNodeSpec(task="merge", role="implement", reducer="dedupe_lines", depends_on=[0, 1]),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason


@pytest.mark.asyncio
async def test_g2_unknown_reducer_also_fails_closed_at_load():
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="worker", role="explore"),
        WorkflowNodeSpec(task="merge", role="implement", reducer="nope", depends_on=[0]),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason


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
async def test_standalone_invalid_workflow_returns_typed_rejection():
    runner = RuntimeRunner(RuntimeOptions(
        provider=_StubProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=_StubOrchestrator(),
        max_tokens=1000,
    ))
    spec = WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="self-cycle", role="implement", depends_on=[0]),
    ])

    outcome = await runner.run_workflow(spec)

    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert outcome.rejection.operation == "start_workflow"
    assert "depends on itself" in outcome.rejection.reason
