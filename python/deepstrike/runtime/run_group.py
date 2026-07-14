"""L1 (RunGroup) — a governance domain shared by N peer agent sessions of one logical run.

The kernel (execution vehicle) is ephemeral and torn down between stateless turns, so the cumulative
budget + membership that must span the whole group live outside any vehicle: in a ``GroupBudgetStore``.
A reservable store atomically holds capacity for each member, seeds its kernel from settled usage plus
earlier reservations, and settles actual consumption when the member ends. Legacy stores retain
read/charge accounting but cannot enforce a concurrent group quota. Per spec §2.5, only
*cumulative* budget is shared this way; instantaneous concurrency stays vehicle-scoped.

Two built-in stores:
- ``InMemoryGroupBudgetStore`` — process-local atomic reservations; fine for one replica / tests.
- ``SessionLogGroupBudgetStore`` — persists the ledger + membership to any ``SessionLog`` (fold-on-read
  under a group-anchor key). It is accounting-only because SessionLog has no compare-and-set API.
"""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from deepstrike.runtime.session_log import SessionLog


@dataclass
class GroupLedger:
    """Cumulative resources spent across a run group."""

    tokens_spent: int = 0
    subagents_spawned: int = 0
    # ③ loop-agent rounds completed across the group (seeds the pacing trap's max_rounds).
    rounds_completed: int = 0


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

    async def read(self, group_id: str) -> GroupLedger:
        """Cumulative spend across the group so far."""
        ...

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0, rounds: int = 0) -> None:
        """Add a member's spend to the group's cumulative totals."""
        ...

    async def join(self, group_id: str, member: GroupMember) -> None:
        """Register a persona session as a member of the group (idempotent by session_id)."""
        ...

    async def members(self, group_id: str) -> list[GroupMember]:
        """All persona sessions of the logical run — the cross-invocation lineage (R2)."""
        ...


@runtime_checkable
class ReservableGroupBudgetStore(GroupBudgetStore, Protocol):
    """Transactional capability required for concurrent group quota enforcement."""

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
    ) -> None: ...

    async def release(self, group_id: str, reservation_id: str) -> None: ...

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

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0, rounds: int = 0) -> None:
        cur = self._ledgers.setdefault(group_id, GroupLedger())
        cur.tokens_spent += max(0, tokens)
        cur.subagents_spawned += max(0, subagents)
        cur.rounds_completed += max(0, rounds)

    async def join(self, group_id: str, member: GroupMember) -> None:
        # Idempotent by session_id (same contract as SessionLogGroupBudgetStore): the FIRST join
        # wins. W-N5 relies on this — a persona joined as "peer" must not be re-tagged "vehicle"
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
                held.tokens_spent += reservation.granted.tokens_spent
                held.subagents_spawned += reservation.granted.subagents_spawned
                held.rounds_completed += reservation.granted.rounds_completed
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
                ledger=ledger,
                granted=GroupLedger(
                    tokens_spent=grant("tokens", ledger.tokens_spent),
                    subagents_spawned=grant("subagents", ledger.subagents_spawned),
                    rounds_completed=grant("rounds", ledger.rounds_completed),
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
                raise ValueError(f"unknown group budget reservation: {reservation_id}")
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


class SessionLogGroupBudgetStore:
    """Persists the group ledger + membership to a ``SessionLog``, keyed by a group-anchor session whose
    id is the group id. Budget/membership rebuild by folding ``group_budget_charged`` /
    ``group_member_joined`` events on read (spec §2.4). This is durable accounting, not concurrent
    quota enforcement; a transactional store must implement reserve/settle/release for that."""

    def __init__(self, log: "SessionLog") -> None:
        self._log = log

    async def read(self, group_id: str) -> GroupLedger:
        tokens = subagents = rounds = 0
        for entry in await self._log.read(group_id):
            if entry.event.get("kind") == "group_budget_charged":
                tokens += entry.event["tokens"]
                subagents += entry.event["subagents"]
                rounds += entry.event.get("rounds") or 0
        return GroupLedger(tokens, subagents, rounds)

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0, rounds: int = 0) -> None:
        event: dict = {
            "kind": "group_budget_charged",
            "tokens": max(0, tokens),
            "subagents": max(0, subagents),
        }
        if rounds:
            event["rounds"] = max(0, rounds)
        await self._log.append(group_id, event)

    async def join(self, group_id: str, member: GroupMember) -> None:
        existing = await self.members(group_id)
        if any(m.session_id == member.session_id for m in existing):
            return
        event: dict = {
            "kind": "group_member_joined",
            "session_id": member.session_id,
            "role": member.role,
        }
        if member.kind:
            event["member_kind"] = member.kind
        await self._log.append(group_id, event)

    async def members(self, group_id: str) -> list[GroupMember]:
        seen: dict[str, GroupMember] = {}
        for entry in await self._log.read(group_id):
            if entry.event.get("kind") == "group_member_joined":
                sid = entry.event["session_id"]
                seen[sid] = GroupMember(sid, entry.event.get("role"), entry.event.get("member_kind"))
        return list(seen.values())


@dataclass(frozen=True)
class GroupBudgetReservation:
    id: str
    group_id: str
    member_id: str
    ledger: GroupLedger
    granted: GroupLedger


def _is_reservable(store: GroupBudgetStore) -> bool:
    return isinstance(store, ReservableGroupBudgetStore)


@dataclass
class RunGroup:
    """Binds a runner to a governance domain: a stable group id + the store its members share."""

    id: str
    budget_store: GroupBudgetStore


class GroupBudgetScope:
    """One member's reservation/accounting lifecycle."""

    def __init__(
        self,
        group: RunGroup,
        ledger: GroupLedger,
        granted: GroupLedger,
        mode: str,
        reservation: GroupBudgetReservation | None = None,
    ) -> None:
        self._group = group
        self.ledger = ledger
        self.granted = granted
        self.mode = mode
        self._reservation = reservation
        self._closed = False

    @classmethod
    async def open(
        cls,
        group: RunGroup,
        member: GroupMember,
        *,
        limits: dict[str, int] | None = None,
        requested: dict[str, int] | None = None,
    ) -> "GroupBudgetScope":
        await group.budget_store.join(group.id, member)
        if requested is not None and _is_reservable(group.budget_store):
            reservation = await group.budget_store.reserve(
                group.id,
                member_id=member.session_id,
                limits=limits or {},
                requested=requested,
            )
            return cls(group, reservation.ledger, reservation.granted, "reserved", reservation)
        granted = GroupLedger(
            tokens_spent=max(0, (requested or {}).get("tokens", 0)),
            subagents_spawned=max(0, (requested or {}).get("subagents", 0)),
            rounds_completed=max(0, (requested or {}).get("rounds", 0)),
        )
        return cls(group, await group.budget_store.read(group.id), granted, "accounting")

    async def settle(self, *, tokens: int = 0, subagents: int = 0, rounds: int = 0) -> None:
        if self._closed:
            return
        self._closed = True
        if self._reservation is not None and _is_reservable(self._group.budget_store):
            await self._group.budget_store.settle(
                self._group.id,
                self._reservation.id,
                tokens=tokens,
                subagents=subagents,
                rounds=rounds,
            )
            return
        if tokens <= 0 and subagents <= 0 and rounds <= 0:
            return
        await self._group.budget_store.charge(
            self._group.id,
            tokens=tokens,
            subagents=subagents,
            rounds=rounds,
        )

    async def release(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self._reservation is not None and _is_reservable(self._group.budget_store):
            await self._group.budget_store.release(self._group.id, self._reservation.id)
