"""L1 (RunGroup) — cumulative token budget spans the governance domain (R2), python parity.

N peer sessions of one logical run share a GroupBudgetStore. Each run is seeded at boot with the
group's cumulative spend, so the run-level max_total_tokens cap is enforced across all members, not
per-vehicle. No run_group => N=1, per-run budget (unchanged).
"""

import asyncio

import pytest

from deepstrike import (
    RuntimeRunner,
    RuntimeOptions,
    InMemorySessionLog,
    LocalExecutionPlane,
    RunGroup,
    GroupMember,
    InMemoryGroupBudgetStore,
    SessionLogGroupBudgetStore,
    GroupBudgetScope,
)
from deepstrike.tools import tool
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta, ToolCallEvent


class _ToolThenTextProvider:
    """Calls a tool on the first turn (loop continues to the budget-check boundary), then final text."""

    def __init__(self) -> None:
        self._turn = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self._turn += 1
        if self._turn == 1:
            yield ToolCallEvent(id="call_1", name="noop", arguments={})
        else:
            yield TextDelta(delta="done")


def noop() -> str:
    """does nothing"""
    return "ok"


def _make_runner(run_group=None, agent_id=None) -> RuntimeRunner:
    plane = LocalExecutionPlane()
    plane.register(tool(noop))
    return RuntimeRunner(RuntimeOptions(
        provider=_ToolThenTextProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=plane,
        max_tokens=4096,
        max_total_tokens=100_000,
        agent_id=agent_id,
        run_group=run_group,
    ))


async def _run_to_done(runner, session_id, goal):
    status, total = "", 0
    async for evt in runner.run(session_id=session_id, goal=goal):
        if getattr(evt, "type", None) == "done":
            status, total = evt.status, evt.total_tokens
    return status, total


@pytest.mark.asyncio
async def test_member_seeded_with_group_spend_hits_shared_cap():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="scenario-1", budget_store=store)
    # Other members already exhausted the 100k cap.
    await store.charge(group.id, tokens=100_000)
    status, _ = await _run_to_done(_make_runner(group), "director", "open the scene")
    assert status == "token_budget"


@pytest.mark.asyncio
async def test_ledger_accumulates_tokens_and_spawns_independently():
    store = InMemoryGroupBudgetStore()
    led0 = await store.read("g")
    assert (led0.tokens_spent, led0.subagents_spawned) == (0, 0)
    await store.charge("g", tokens=100)
    await store.charge("g", subagents=2)
    await store.charge("g", tokens=50, subagents=1)
    led = await store.read("g")
    assert (led.tokens_spent, led.subagents_spawned) == (150, 3)


@pytest.mark.asyncio
async def test_tracks_membership_lineage():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="scenario-3", budget_store=store)
    await _run_to_done(_make_runner(group, "director"), "director", "beat 1")
    await _run_to_done(_make_runner(group, "role-npc"), "role-npc", "beat 2")
    await _run_to_done(_make_runner(group, "director"), "director", "beat 3")  # rejoin idempotent
    members = await store.members(group.id)
    assert sorted(m.session_id for m in members) == ["director", "role-npc"]
    assert next(m for m in members if m.session_id == "director").role == "director"


@pytest.mark.asyncio
async def test_sessionlog_store_persists_across_instances():
    log = InMemorySessionLog()
    writer = SessionLogGroupBudgetStore(log)
    await writer.join("run-x", GroupMember("director", "director"))
    await writer.charge("run-x", tokens=1200, subagents=2)
    await writer.charge("run-x", tokens=800, subagents=1)

    reader = SessionLogGroupBudgetStore(log)  # different instance, same log
    led = await reader.read("run-x")
    assert (led.tokens_spent, led.subagents_spawned) == (2000, 3)
    assert [m.session_id for m in await reader.members("run-x")] == ["director"]
    await reader.join("run-x", GroupMember("director", "director"))  # idempotent
    assert len(await reader.members("run-x")) == 1


@pytest.mark.asyncio
async def test_without_group_same_run_completes():
    status, _ = await _run_to_done(_make_runner(None), "solo", "open the scene")
    assert status == "completed"


@pytest.mark.asyncio
async def test_charges_accumulate_into_group_total():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="scenario-2", budget_store=store)
    assert (await store.read(group.id)).tokens_spent == 0
    status1, t1 = await _run_to_done(_make_runner(group), "p1", "first beat")
    assert status1 == "completed"
    assert t1 > 0
    assert (await store.read(group.id)).tokens_spent == t1
    _, t2 = await _run_to_done(_make_runner(group), "p2", "second beat")
    assert (await store.read(group.id)).tokens_spent == t1 + t2
@pytest.mark.asyncio
async def test_atomically_reserves_capacity_across_concurrent_members():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="concurrent", budget_store=store)

    first, second = await asyncio.gather(
        GroupBudgetScope.open(group, GroupMember("a"), limits={"tokens": 100}, requested={"tokens": 100}),
        GroupBudgetScope.open(group, GroupMember("b"), limits={"tokens": 100}, requested={"tokens": 100}),
    )

    assert first.mode == "reserved"
    assert first.granted.tokens_spent == 100
    assert second.granted.tokens_spent == 0
    assert second.ledger.tokens_spent == 100

    await first.settle(tokens=60)
    await second.release()
    assert (await store.read(group.id)).tokens_spent == 60


@pytest.mark.asyncio
async def test_non_transactional_store_is_accounting_only():
    store = SessionLogGroupBudgetStore(InMemorySessionLog())
    scope = await GroupBudgetScope.open(
        RunGroup(id="legacy", budget_store=store),
        GroupMember("a"),
        limits={"tokens": 100},
        requested={"tokens": 100},
    )

    assert scope.mode == "accounting"
    assert scope.granted.tokens_spent == 100


@pytest.mark.asyncio
async def test_reservation_remains_retryable_when_settlement_fails():
    class FlakyStore(InMemoryGroupBudgetStore):
        def __init__(self):
            super().__init__()
            self.attempts = 0

        async def settle(self, group_id, reservation_id, **actual):
            self.attempts += 1
            if self.attempts == 1:
                raise RuntimeError("temporary store failure")
            await super().settle(group_id, reservation_id, **actual)

    store = FlakyStore()
    scope = await GroupBudgetScope.open(
        RunGroup(id="retry", budget_store=store),
        GroupMember("a"),
        limits={"tokens": 100},
        requested={"tokens": 100},
    )

    with pytest.raises(RuntimeError, match="temporary store failure"):
        await scope.settle(tokens=40)
    await scope.settle(tokens=40)

    assert (await store.read("retry")).tokens_spent == 40
