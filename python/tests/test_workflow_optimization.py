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
from deepstrike.providers.base import ModelMessage
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.run_group import GroupMember
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


# ── W-N1: workflow nodes get tools (trusted inherit; quarantined stay deny-all) ──────────────────


class _NodeProvider:
    """Call 1: try the parent's `ping` tool; call 2+: final text."""

    def __init__(self) -> None:
        self._call = 0

    async def complete(self, context, tools, extensions=None):
        return ModelMessage(role="assistant", content="done")

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
        baseline_tool_ids=["ping"],
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
        return ModelMessage(role="assistant", content="done")

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
async def test_resume_restores_only_explicitly_tagged_peers():
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

    untagged = InMemoryGroupBudgetStore()
    await untagged.join("g2", GroupMember("solo"))
    strict_session = await ReactiveSession.resume(
        run_group=RunGroup(id="g2", budget_store=untagged),
        turn_policy=lambda event, peers, state: [],
        make_runner=_no_runner,
    )
    assert strict_session.peers() == []
