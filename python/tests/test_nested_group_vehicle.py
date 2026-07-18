"""Regression: a nested vehicle (a sub-agent spawned by the SubAgentOrchestrator while its parent
already holds a RunGroup reservation) must NOT re-reserve the group's token axis.

Before the fix the child opened its GroupBudgetScope with the full per-vehicle request, the
peer-competition formula squeezed its grant to 0 tokens against the parent's held reservation, and
the kernel's configure_run then stripped the child's first-turn tool list — the child model saw
"no tools available".

After the fix ``_build_child_opts`` sets ``nested_group_vehicle=True``, so the child's scope opens
with an EMPTY request (``limits={}, requested={}``) → granted ``GroupBudgetGrant()`` (no axis). The
child joins for lineage/settlement only; the parent's held reservation can no longer squeeze it, and
its first-turn tools (here ``noop``) survive.

Mirrors the Node ``nested-group-vehicle.test.ts``. The parent kernel is injected (as in
``test_bootstrap_workflow.py``) with ``noop`` mounted as a capability so the kernel-computed spawn
manifest carries it through ``spec.capability_filter``. A recording store captures every reservation
so the child's empty grant is directly assertable.
"""
from __future__ import annotations

import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
)
from deepstrike._kernel import KernelRuntime, LoopPolicy
from deepstrike.providers.base import Message
from deepstrike.providers.stream import DoneEvent, ErrorEvent, TextDelta
from deepstrike.runtime.kernel_step import kernel_action
from deepstrike.runtime.run_group import (
    GroupBudgetGrant,
    GroupBudgetReservation,
    GroupBudgetScope,
    GroupMember,
    InMemoryGroupBudgetStore,
    RunGroup,
)
from deepstrike.tools import tool
from deepstrike.types.agent import AgentCapabilityFilter, AgentIdentity, AgentRunSpec


class _RecordingProvider:
    """Records the tool names it is handed on every LLM call, then completes the turn with text."""

    def __init__(self) -> None:
        self.calls: list[list[str]] = []

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls.append([t.name for t in tools])
        yield TextDelta(delta="done")


class _RecordingStore(InMemoryGroupBudgetStore):
    """Captures every reservation the store hands out so the child's empty grant is assertable."""

    def __init__(self) -> None:
        super().__init__()
        self.recorded: list[GroupBudgetReservation] = []

    async def reserve(self, group_id, *, member_id, limits, requested):
        reservation = await super().reserve(
            group_id, member_id=member_id, limits=limits, requested=requested
        )
        self.recorded.append(reservation)
        return reservation


def _noop() -> str:
    """Do nothing."""
    return "ok"


@pytest.mark.asyncio
async def test_nested_vehicle_joins_group_without_reserving_token_axis():
    store = _RecordingStore()
    group = RunGroup(id="nested", budget_store=store)

    # (1) The parent holds a FULL token reservation and never settles it — the exact condition that
    # squeezed a re-reserving child to a zero-token grant.
    parent_scope = await GroupBudgetScope.open(
        group,
        GroupMember("parent"),
        limits={"tokens": 100_000},
        requested={"tokens": 100_000},
    )
    assert parent_scope.granted.tokens == 100_000

    # (2) Parent runner: shares the group + the noop-bearing plane with its spawned child.
    provider = _RecordingProvider()
    plane = LocalExecutionPlane()
    plane.register(tool(_noop))
    noop_schema = next(s for s in plane.schemas() if s.name == "_noop")
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=plane,
        max_tokens=4096,
        max_total_tokens=100_000,
        run_group=group,
        agent_id="parent",
    ))

    # Inject an active parent kernel (spawn_sub_agent requires a live parent run). Mount `_noop` as a
    # capability so the kernel-computed spawn manifest can carry it through the capability filter —
    # set_tools alone populates sm.tools, not the ctx.capabilities the spawn manifest reads.
    runtime = KernelRuntime(LoopPolicy(max_tokens=128_000))
    kernel_action(runtime, [], {"kind": "start_run", "task": {"goal": "parent", "criteria": []}})
    runner._active_kernel = runtime
    runner._current_session_id = "parent"
    runner._pending_observations = []
    runner.mount_tool(noop_schema)

    # (3) Spawn the child through the full kernel path. `capability_filter.allowed_ids` gates the
    # kernel spawn manifest's permitted_capability_ids to just `_noop` (empty allow-list ⇒ deny-all).
    spec = AgentRunSpec(
        identity=AgentIdentity(agent_id="worker", session_id="worker-child", is_sub_agent=True),
        role="implement",
        isolation="shared",
        goal="do the work",
        capability_filter=AgentCapabilityFilter(allowed_ids=["_noop"]),
    )
    events = [event async for event in await runner.spawn_sub_agent(spec)]

    # (a) The child completed cleanly — no error event, a terminal done with status "completed".
    assert not any(isinstance(e, ErrorEvent) for e in events)
    done = next((e for e in events if isinstance(e, DoneEvent)), None)
    assert done is not None
    assert done.status == "completed"

    # (b) The child's FIRST LLM call still saw `_noop` — the thing the zero-token grant used to strip.
    assert len(provider.calls) > 0
    assert "_noop" in provider.calls[0]

    # (c) The child's reservation reserved NO axis: granted is the empty grant (all axes None).
    child_reservation = next((r for r in store.recorded if r.member_id == "worker-child"), None)
    assert child_reservation is not None
    assert child_reservation.granted == GroupBudgetGrant()

    # (d) The parent's full reservation is still held (never settled/released): a fresh token request
    # is squeezed to 0 because the parent still occupies the whole 100_000 in the ledger.
    assert parent_scope.closed is False
    probe = await GroupBudgetScope.open(
        group,
        GroupMember("probe"),
        limits={"tokens": 100_000},
        requested={"tokens": 100_000},
    )
    assert probe.granted.tokens == 0
    await probe.release()
