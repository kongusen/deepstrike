"""Dynamic-workflow optimization batch: node caps and loop pacing parity."""

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
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.run_group import GroupMember
from deepstrike.runtime.workflow_control_flow import dependency_outputs_note
from deepstrike.tools import tool
from deepstrike.types.agent import workflow_node_to_spec

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
    assert [n.node_id for n in outcome.node_outcomes if n.status in ("completed", "completed_partial")] == ["wf-node0"]
    assert pings["n"] == 1  # pre-W-N1 this was 0: the missing grant list ran every node TOOL-LESS


@pytest.mark.asyncio
async def test_quarantined_workflow_node_fails_closed_until_canonical_trust():
    pings = {"n": 0}
    runner = _tooled_runner(pings)
    outcome = await runner.run_workflow(WorkflowSpec(nodes=[
        WorkflowNodeSpec(task="try the ping tool", role="explore",
                         isolation="read_only", trust="quarantined"),
    ]))
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: trust" in outcome.rejection.reason
    assert pings["n"] == 0


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
async def test_pace_continue_then_stop_fails_closed_on_loop_kind():
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
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason
    assert await session_log.read("wfloop-wf-node0") == []


@pytest.mark.asyncio
async def test_iteration_that_never_paces_also_fails_closed_on_loop_kind():
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
    assert outcome.node_outcomes == []
    assert outcome.rejection is not None
    assert "absent from canonical WorkflowNode: kind" in outcome.rejection.reason
    assert await session_log.read("wfsilent-wf-node0") == []


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
