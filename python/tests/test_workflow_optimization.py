"""Dynamic-workflow optimization batch (W-N1/N2, W-1 resume signal replay, per-node caps,
DW-3/W-N6 loop pacing) — python parity with node/tests/workflow-optimization.test.ts."""

import json

import pytest

from deepstrike import (
    InMemoryGroupBudgetStore,
    InMemorySessionLog,
    LocalExecutionPlane,
    ReactiveSession,
    RunGroup,
    RuntimeOptions,
    RuntimeRunner,
    WorkflowNodeSpec,
    WorkflowSpec,
    WorkflowSpawnInfo,
    workflow_node_spec_to_kernel,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.run_group import GroupMember
from deepstrike.runtime.workflow_control_flow import dependency_outputs_note
from deepstrike.runtime.kernel_step import _kernel_step
from deepstrike.tools import tool
from deepstrike.types.agent import workflow_node_to_spec


def _step(rt: KernelRuntime, event: dict) -> dict:
    return _kernel_step(rt, event)


def _batch_of(step: dict) -> list:
    action = next((a for a in step.get("actions", []) if a.get("kind") == "spawn_workflow"), None)
    return (action or {}).get("nodes") or []


def _accept_batch(rt: KernelRuntime, step: dict) -> None:
    action = next(a for a in step.get("actions", []) if a.get("kind") == "spawn_workflow")
    _step(rt, {
        "kind": "workflow_spawn_result",
        "effect_id": action["effect_id"],
        "started_agent_ids": [node["agent_id"] for node in action.get("nodes", [])],
        "failures": [],
    })


# ── W-1: resume replays classify control flow over the ABI ──────────────────────────────────────


def test_w1_recorded_classify_branch_reprunes_rejected_branch_on_resume():
    rt = KernelRuntime(LoopPolicy(max_tokens=8000, max_turns=10))
    _step(rt, {"kind": "start_run", "task": {"goal": "resume classify", "criteria": []}})
    out = _step(rt, {
        "kind": "load_workflow",
        "spec": {
            "nodes": [
                {
                    "task": {"goal": "route", "criteria": []},
                    "role": "plan", "isolation": "shared", "context_inheritance": "none",
                    "kind": {"type": "classify", "branches": [
                        {"label": "a", "nodes": [1]}, {"label": "b", "nodes": [2]},
                    ]},
                },
                {"task": {"goal": "on a", "criteria": []}, "role": "implement",
                 "isolation": "shared", "context_inheritance": "none", "depends_on": [0]},
                {"task": {"goal": "on b", "criteria": []}, "role": "implement",
                 "isolation": "shared", "context_inheritance": "none", "depends_on": [0]},
            ],
        },
        "parent_session_id": "sess",
        # W-1: the signal-carrying record — the classifier chose "a" pre-crash.
        "resumed_results": [{"agent_id": "wf-node0", "classify_branch": "a"}],
    })
    # Only the chosen branch spawns; the rejected branch stays pruned across resume.
    batch = _batch_of(out)
    assert [n["agent_id"] for n in batch] == ["wf-node1"]


# ── W-N2 / W-N7: spawn descriptors carry data edges and per-node caps ────────────────────────────


def test_node_spec_to_kernel_emits_caps_and_node_to_spec_maps_them_back():
    kernel_json = workflow_node_spec_to_kernel(WorkflowNodeSpec(
        task="expensive", role="implement", token_budget=5000, max_turns=4, max_wall_ms=30_000,
    ))
    assert kernel_json["max_turns"] == 4
    assert kernel_json["max_wall_ms"] == 30_000

    spec = workflow_node_to_spec(
        WorkflowSpawnInfo(
            agent_id="wf-node0", goal="g", role="implement", isolation="shared",
            context_inheritance="none", token_budget=5000, max_turns=4, max_wall_ms=30_000,
        ),
        "parent",
    )
    assert spec.max_turns == 4
    assert spec.max_wall_ms == 30_000
    assert spec.token_budget == 5000


def test_plain_dependent_node_spawn_info_carries_dependency_agent_ids():
    rt = KernelRuntime(LoopPolicy(max_tokens=8000, max_turns=10))
    _step(rt, {"kind": "start_run", "task": {"goal": "deps", "criteria": []}})
    out = _step(rt, {
        "kind": "load_workflow",
        "spec": {
            "nodes": [
                {"task": {"goal": "w0", "criteria": []}, "role": "explore",
                 "isolation": "read_only", "context_inheritance": "none"},
                {"task": {"goal": "w1", "criteria": []}, "role": "explore",
                 "isolation": "read_only", "context_inheritance": "none"},
                {"task": {"goal": "synth", "criteria": []}, "role": "plan",
                 "isolation": "shared", "context_inheritance": "none", "depends_on": [0, 1]},
            ],
        },
        "parent_session_id": "sess",
    })
    workers = _batch_of(out)
    assert [n.get("input_agent_ids") or [] for n in workers] == [[], []]
    _accept_batch(rt, out)

    # Complete both workers → the synthesizer spawns WITH its data edges.
    def _mk_result(agent_id: str) -> dict:
        return {
            "kind": "sub_agent_completed",
            "result": {"agent_id": agent_id, "result": {
                "termination": "completed",
                "final_message": {"role": "assistant", "content": f"{agent_id} out"},
                "turns_used": 1, "total_tokens_used": 1,
            }},
        }

    _step(rt, _mk_result("wf-node0"))
    after = _step(rt, _mk_result("wf-node1"))
    synth = _batch_of(after)
    assert [n["agent_id"] for n in synth] == ["wf-node2"]
    assert synth[0]["input_agent_ids"] == ["wf-node0", "wf-node1"]


def test_dependency_outputs_note_formats_clips_and_skips_empty():
    outputs = {
        "wf-node0": "alpha findings",
        "wf-node1": "x" * 9000,
    }
    note = dependency_outputs_note(["wf-node0", "wf-node1", "wf-node-missing"], outputs, 100)
    assert "[dependency wf-node0 output]\nalpha findings" in note
    assert "…[truncated]" in note
    assert "wf-node-missing" not in note
    assert dependency_outputs_note([], outputs) == ""
    assert dependency_outputs_note(None, outputs) == ""


# ── W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all) ──────────────────


class _NodeProvider:
    """Call 1: try the parent's `ping` tool; call 2+: final text."""

    def __init__(self) -> None:
        self._call = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self._call += 1
        if self._call == 1:
            yield ToolCallEvent(id=f"t-{self._call}", name="ping", arguments={})
            return
        yield TextDelta(delta="node done")


def _tooled_runner(pings: dict) -> RuntimeRunner:
    def ping() -> str:
        """ping the host"""
        pings["n"] += 1
        return "pong"

    plane = LocalExecutionPlane()
    plane.register(tool(ping))
    return RuntimeRunner(RuntimeOptions(
        provider=_NodeProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=16_000,
    ))


@pytest.mark.asyncio
async def test_trusted_workflow_node_can_call_parent_registered_tools():
    pings = {"n": 0}
    runner = _tooled_runner(pings)
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="use the ping tool once, then stop", role="implement"),
    ]))
    assert outcome["completed"] == ["wf-node0"]
    assert pings["n"] == 1  # pre-W-N1 this was 0: the missing grant list ran every node TOOL-LESS


@pytest.mark.asyncio
async def test_quarantined_workflow_node_stays_deny_all_filtered():
    pings = {"n": 0}
    runner = _tooled_runner(pings)
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="try the ping tool", role="explore",
                         isolation="read_only", trust="quarantined"),
    ]))
    assert outcome["completed"] == ["wf-node0"]
    assert pings["n"] == 0  # untrusted-content reader: no tool reaches the host


# ── DW-3/W-N6: loop nodes pace through the kernel trap on ONE stable session ─────────────────────


class _PacingLoopProvider:
    """Per ITERATION the child makes two calls: propose a pace verb, then file the report turn."""

    def __init__(self, verbs: list[str]) -> None:
        self._verbs = verbs
        self._call = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self._call += 1
        iteration = (self._call + 1) // 2 - 1
        if self._call % 2 == 1:
            yield ToolCallEvent(
                id=f"pace-{self._call}", name="pace",
                arguments={
                    "next": self._verbs[min(iteration, len(self._verbs) - 1)],
                    "reason": f"iter {iteration}",
                },
            )
            return
        yield TextDelta(delta=f"iteration {iteration} report")


@pytest.mark.asyncio
async def test_pace_continue_then_stop_drives_iterations_on_one_stable_session():
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_PacingLoopProvider(["continue", "stop"]),
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=16_000,
    ))
    outcome = await runner.run_workflow(
        WorkflowSpec(nodes=[
            WorkflowNodeSpec(task="polish until done", role="implement", loop={"max_iters": 5}),
        ]),
        session_id="wfloop",
    )
    assert outcome["completed"] == ["wf-node0"]
    # The pace verb ended the loop at 2 iterations, well before max_iters=5.
    loop_session = await session_log.read("wfloop-wf-node0")
    starts = [e for e in loop_session if e.event.get("kind") == "run_started"]
    assert len(starts) == 2  # W-N6: BOTH iterations ran under the ONE stable session id
    # No per-iteration session fragments.
    assert await session_log.read("wfloop-wf-node0-i0") == []
    assert await session_log.read("wfloop-wf-node0-i1") == []


@pytest.mark.asyncio
async def test_iteration_that_never_paces_completes_the_loop():
    """Silence = done (the CC contract), not run-to-cap."""
    class _Silent:
        async def complete(self, context, tools, extensions=None):
            return Message(role="assistant", content="done")

        async def stream(self, context, tools, extensions=None, state=None):
            yield TextDelta(delta="all done in one pass")

    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_Silent(),
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=16_000,
    ))
    outcome = await runner.run_workflow(
        WorkflowSpec(nodes=[
            WorkflowNodeSpec(task="one-shot polish", role="implement", loop={"max_iters": 4}),
        ]),
        session_id="wfsilent",
    )
    assert outcome["completed"] == ["wf-node0"]
    # default_action=stop: exactly ONE iteration ran (the kernel's pace fallback said stop).
    starts = [e for e in await session_log.read("wfsilent-wf-node0") if e.event.get("kind") == "run_started"]
    assert len(starts) == 1


# ── W-N5: ReactiveSession.resume rebuilds peers, not vehicles ────────────────────────────────────


@pytest.mark.asyncio
async def test_resume_filters_vehicle_members_and_keeps_legacy_memberships_whole():
    store = InMemoryGroupBudgetStore()
    await store.join("g1", GroupMember("alice", "reviewer", kind="peer"))
    await store.join("g1", GroupMember("wf-abc123", "loop", kind="vehicle"))
    await store.join("g1", GroupMember("bob", kind="peer"))

    def _no_runner(persona_id, shared):
        raise AssertionError("not driven in this test")

    session = await ReactiveSession.resume(
        run_group=RunGroup(id="g1", budget_store=store),
        turn_policy=lambda event, peers, state: [],
        make_runner=_no_runner,
    )
    assert sorted(session.peers()) == ["alice", "bob"]

    # Legacy: nothing tagged → every member resumes as a peer (old behavior preserved).
    legacy = InMemoryGroupBudgetStore()
    await legacy.join("g2", GroupMember("solo"))
    legacy_session = await ReactiveSession.resume(
        run_group=RunGroup(id="g2", budget_store=legacy),
        turn_policy=lambda event, peers, state: [],
        make_runner=_no_runner,
    )
    assert legacy_session.peers() == ["solo"]
