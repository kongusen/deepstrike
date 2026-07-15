"""L1 (RunGroup) — a governance domain shared by N peer agent sessions of one logical run.

The kernel (execution vehicle) is ephemeral and torn down between stateless turns, so the cumulative
budget + membership that must span the whole group live outside any vehicle: in a ``GroupBudgetStore``.
Every store atomically holds capacity for each member and settles actual consumption. Per spec §2.5, only
*cumulative* budget is shared this way; instantaneous concurrency stays vehicle-scoped.

``InMemoryGroupBudgetStore`` provides process-local atomic reservations for one replica / tests.
"""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass
from typing import Protocol, runtime_checkable


@dataclass
class GroupLedger:
    """Cumulative resources spent across a run group."""

    tokens_spent: int = 0
    subagents_spawned: int = 0
    # ③ loop-agent rounds completed across the group (seeds the pacing trap's max_rounds).
    rounds_completed: int = 0


@dataclass(frozen=True)
class GroupBudgetGrant:
    """Capacity granted on the axes this member actually requested."""

    tokens: int | None = None
    subagents: int | None = None
    rounds: int | None = None


@dataclass
class GroupMember:
    """A persona session that participated in the logical run (process-table lineage)."""

    session_id: str
    role: str | None = None
    # W-N5: what this member IS in the lineage — a "peer" persona (ReactiveSession.add_peer) vs a
    # "vehicle" session (run()/run_workflow envelopes, workflow-node children, loop iterations).
    # ``ReactiveSession.resume()`` rebuilds the peer set from "peer" members only, so DAG-in-Peer
    # usage can't resurrect phantom ``wf-node*`` personas. None (legacy) = unknown.
    kind: str | None = None  # "peer" | "vehicle"


@runtime_checkable
class GroupBudgetStore(Protocol):
    """Cumulative-budget + membership ledger shared by the members of a run group."""

    async def join(self, group_id: str, member: GroupMember) -> None:
        """Register a persona session as a member of the group (idempotent by session_id)."""
        ...

    async def members(self, group_id: str) -> list[GroupMember]:
        """All persona sessions of the logical run — the cross-invocation lineage (R2)."""
        ...


    async def reserve(
        self,
        group_id: str,
        *,
        member_id: str,
        limits: dict[str, int],
        requested: dict[str, int],
    ) -> "GroupBudgetReservation": ...

    async def settle(
        self,
        group_id: str,
        reservation_id: str,
        *,
        tokens: int = 0,
        subagents: int = 0,
        rounds: int = 0,
    ) -> None:
        """Idempotently replace a reservation with actual usage."""
        ...

    async def release(self, group_id: str, reservation_id: str) -> None:
        """Idempotently discard an unused reservation."""
        ...

class InMemoryGroupBudgetStore:
    """Process-local default store. One ledger + member set per group id."""

    def __init__(self) -> None:
        self._ledgers: dict[str, GroupLedger] = {}
        self._members: dict[str, dict[str, GroupMember]] = {}
        self._reservations: dict[str, dict[str, GroupBudgetReservation]] = {}
        self._locks: dict[str, asyncio.Lock] = {}

    async def read(self, group_id: str) -> GroupLedger:
        cur = self._ledgers.get(group_id)
        return GroupLedger(cur.tokens_spent, cur.subagents_spawned, cur.rounds_completed) if cur else GroupLedger()

    async def join(self, group_id: str, member: GroupMember) -> None:
        # Idempotent by session_id: the FIRST join wins. W-N5 relies on this — a persona joined as
        # "peer" must not be re-tagged "vehicle"
        # when its own turn's run() joins the same session id.
        self._members.setdefault(group_id, {}).setdefault(member.session_id, member)

    async def members(self, group_id: str) -> list[GroupMember]:
        return list(self._members.get(group_id, {}).values())

    async def reserve(
        self,
        group_id: str,
        *,
        member_id: str,
        limits: dict[str, int],
        requested: dict[str, int],
    ) -> "GroupBudgetReservation":
        async with self._locks.setdefault(group_id, asyncio.Lock()):
            settled = await self.read(group_id)
            held = GroupLedger()
            for reservation in self._reservations.get(group_id, {}).values():
                held.tokens_spent += reservation.granted.tokens or 0
                held.subagents_spawned += reservation.granted.subagents or 0
                held.rounds_completed += reservation.granted.rounds or 0
            ledger = GroupLedger(
                settled.tokens_spent + held.tokens_spent,
                settled.subagents_spawned + held.subagents_spawned,
                settled.rounds_completed + held.rounds_completed,
            )

            def grant(axis: str, used: int) -> int:
                wanted = max(0, requested.get(axis, 0))
                limit = limits.get(axis)
                return max(0, min(wanted, wanted if limit is None else limit - used))

            reservation = GroupBudgetReservation(
                id=str(uuid.uuid4()),
                group_id=group_id,
                member_id=member_id,
                granted=GroupBudgetGrant(
                    tokens=grant("tokens", ledger.tokens_spent) if "tokens" in requested else None,
                    subagents=(
                        grant("subagents", ledger.subagents_spawned)
                        if "subagents" in requested else None
                    ),
                    rounds=grant("rounds", ledger.rounds_completed) if "rounds" in requested else None,
                ),
            )
            self._reservations.setdefault(group_id, {})[reservation.id] = reservation
            return reservation

    async def settle(
        self,
        group_id: str,
        reservation_id: str,
        *,
        tokens: int = 0,
        subagents: int = 0,
        rounds: int = 0,
    ) -> None:
        async with self._locks.setdefault(group_id, asyncio.Lock()):
            reservations = self._reservations.get(group_id, {})
            if reservations.pop(reservation_id, None) is None:
                return
            if not reservations:
                self._reservations.pop(group_id, None)
            cur = self._ledgers.setdefault(group_id, GroupLedger())
            cur.tokens_spent += max(0, tokens)
            cur.subagents_spawned += max(0, subagents)
            cur.rounds_completed += max(0, rounds)

    async def release(self, group_id: str, reservation_id: str) -> None:
        async with self._locks.setdefault(group_id, asyncio.Lock()):
            reservations = self._reservations.get(group_id, {})
            reservations.pop(reservation_id, None)
            if not reservations:
                self._reservations.pop(group_id, None)


@dataclass(frozen=True)
class GroupBudgetReservation:
    id: str
    group_id: str
    member_id: str
    granted: GroupBudgetGrant


@dataclass
class RunGroup:
    """Binds a runner to a governance domain: a stable group id + the store its members share."""

    id: str
    budget_store: GroupBudgetStore


class GroupBudgetScope:
    """One member's reservation lifecycle."""

    def __init__(
        self,
        group: RunGroup,
        granted: GroupBudgetGrant,
        reservation_id: str,
    ) -> None:
        self._group = group
        self.granted = granted
        self.reservation_id = reservation_id
        self._closed = False

    @classmethod
    async def open(
        cls,
        group: RunGroup,
        member: GroupMember,
        *,
        limits: dict[str, int],
        requested: dict[str, int],
    ) -> "GroupBudgetScope":
        await group.budget_store.join(group.id, member)
        reservation = await group.budget_store.reserve(
            group.id,
            member_id=member.session_id,
            limits=limits,
            requested=requested,
        )
        return cls(group, reservation.granted, reservation.id)

    async def settle(self, *, tokens: int = 0, subagents: int = 0, rounds: int = 0) -> None:
        if self._closed:
            return
        await self._group.budget_store.settle(
            self._group.id,
            self.reservation_id,
            tokens=tokens,
            subagents=subagents,
            rounds=rounds,
        )
        self._closed = True

    async def release(self) -> None:
        if self._closed:
            return
        await self._group.budget_store.release(self._group.id, self.reservation_id)
        self._closed = True

    @property
    def closed(self) -> bool:
        return self._closed
