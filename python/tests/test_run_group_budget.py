import asyncio

import pytest

from deepstrike import (
    GroupBudgetScope,
    GroupMember,
    InMemoryGroupBudgetStore,
    InMemorySessionLog,
    LocalExecutionPlane,
    RunGroup,
    RuntimeOptions,
    RuntimeRunner,
)
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools import tool


class _ToolThenTextProvider:
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
    """Do nothing."""
    return "ok"


def _make_runner(run_group=None, agent_id=None, kernel_reliability=None) -> RuntimeRunner:
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
        kernel_reliability=kernel_reliability,
    ))


async def _run_to_done(runner, session_id):
    status, total = "", 0
    async for event in runner.run(session_id=session_id, goal="open the scene"):
        if getattr(event, "type", None) == "done":
            status, total = event.status, event.total_tokens
    return status, total


@pytest.mark.asyncio
async def test_reservations_are_atomic_without_overselling():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="concurrent", budget_store=store)
    first, second = await asyncio.gather(
        GroupBudgetScope.open(group, GroupMember("a"), limits={"tokens": 100}, requested={"tokens": 100}),
        GroupBudgetScope.open(group, GroupMember("b"), limits={"tokens": 100}, requested={"tokens": 100}),
    )

    assert first.granted.tokens == 100
    assert second.granted.tokens == 0
    await first.settle(tokens=60)
    await second.release()
    assert (await store.read(group.id)).tokens_spent == 60


@pytest.mark.asyncio
async def test_unrequested_axis_is_not_lowered_as_zero_capacity():
    store = InMemoryGroupBudgetStore()
    scope = await GroupBudgetScope.open(
        RunGroup(id="partial", budget_store=store),
        GroupMember("a"),
        limits={"subagents": 4},
        requested={"subagents": 2},
    )
    assert scope.granted.tokens is None
    assert scope.granted.subagents == 2
    assert scope.granted.rounds is None
    await scope.release()


@pytest.mark.asyncio
async def test_failed_settlement_keeps_reservation_retryable():
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
    assert not scope.closed
    await scope.settle(tokens=40)
    assert (await store.read("retry")).tokens_spent == 40


@pytest.mark.asyncio
async def test_runner_retries_terminal_settlement_from_reliability_policy():
    class FlakyStore(InMemoryGroupBudgetStore):
        def __init__(self):
            super().__init__()
            self.attempts = 0

        async def settle(self, group_id, reservation_id, **actual):
            self.attempts += 1
            if self.attempts == 1:
                raise RuntimeError("temporary store failure")
            await super().settle(group_id, reservation_id, **actual)

    from deepstrike import KernelReliability

    store = FlakyStore()
    runner = _make_runner(
        RunGroup(id="host-retry", budget_store=store),
        kernel_reliability=KernelReliability(host_effect_retry_attempts=1),
    )
    status, _ = await _run_to_done(runner, "member")
    assert status == "completed"
    assert store.attempts == 2


@pytest.mark.asyncio
async def test_exhausted_group_is_enforced_as_zero_capacity_grant():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="exhausted", budget_store=store)
    seed = await GroupBudgetScope.open(
        group,
        GroupMember("prior-member"),
        limits={"tokens": 100_000},
        requested={"tokens": 100_000},
    )
    await seed.settle(tokens=100_000)
    status, _ = await _run_to_done(_make_runner(group), "director")
    assert status == "token_budget"


@pytest.mark.asyncio
async def test_kernel_usage_is_settled_and_membership_is_preserved():
    store = InMemoryGroupBudgetStore()
    group = RunGroup(id="usage", budget_store=store)
    status1, tokens1 = await _run_to_done(_make_runner(group, "director"), "director")
    status2, tokens2 = await _run_to_done(_make_runner(group, "critic"), "critic")
    assert (status1, status2) == ("completed", "completed")
    assert (await store.read(group.id)).tokens_spent == tokens1 + tokens2
    assert sorted(member.session_id for member in await store.members(group.id)) == ["critic", "director"]


@pytest.mark.asyncio
async def test_no_group_keeps_per_vehicle_budget():
    status, _ = await _run_to_done(_make_runner(), "solo")
    assert status == "completed"
