"""L1 (RunGroup) — a governance domain shared by N peer agent sessions of one logical run.

The kernel (execution vehicle) is ephemeral and torn down between stateless turns, so the cumulative
budget + membership that must span the whole group live outside any vehicle: in a ``GroupBudgetStore``.
Each member's run is seeded at boot with the group's accumulated spend (tokens + sub-agent spawns) so
the run-level token cap and the cumulative spawn cap are enforced across all members, registers itself
as a member (lineage), and charges its own consumption back when it ends. Per spec §2.5, only
*cumulative* budget is shared this way; instantaneous concurrency stays vehicle-scoped.

Two built-in stores:
- ``InMemoryGroupBudgetStore`` — process-local; fine for a single replica / tests.
- ``SessionLogGroupBudgetStore`` — persists the ledger + membership to any ``SessionLog`` (fold-on-read
  under a group-anchor key), so a logical run's governance + lineage survive process boundaries and
  span replicas when backed by a durable ``SessionLog``.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Protocol, runtime_checkable

if TYPE_CHECKING:
    from deepstrike.runtime.session_log import SessionLog


@dataclass
class GroupLedger:
    """Cumulative resources spent across a run group."""

    tokens_spent: int = 0
    subagents_spawned: int = 0


@dataclass
class GroupMember:
    """A persona session that participated in the logical run (process-table lineage)."""

    session_id: str
    role: str | None = None


@runtime_checkable
class GroupBudgetStore(Protocol):
    """Cumulative-budget + membership ledger shared by the members of a run group."""

    async def read(self, group_id: str) -> GroupLedger:
        """Cumulative spend across the group so far."""
        ...

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0) -> None:
        """Add a member's spend to the group's cumulative totals."""
        ...

    async def join(self, group_id: str, member: GroupMember) -> None:
        """Register a persona session as a member of the group (idempotent by session_id)."""
        ...

    async def members(self, group_id: str) -> list[GroupMember]:
        """All persona sessions of the logical run — the cross-invocation lineage (R2)."""
        ...


class InMemoryGroupBudgetStore:
    """Process-local default store. One ledger + member set per group id."""

    def __init__(self) -> None:
        self._ledgers: dict[str, GroupLedger] = {}
        self._members: dict[str, dict[str, GroupMember]] = {}

    async def read(self, group_id: str) -> GroupLedger:
        cur = self._ledgers.get(group_id)
        return GroupLedger(cur.tokens_spent, cur.subagents_spawned) if cur else GroupLedger()

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0) -> None:
        cur = self._ledgers.setdefault(group_id, GroupLedger())
        cur.tokens_spent += max(0, tokens)
        cur.subagents_spawned += max(0, subagents)

    async def join(self, group_id: str, member: GroupMember) -> None:
        self._members.setdefault(group_id, {})[member.session_id] = member

    async def members(self, group_id: str) -> list[GroupMember]:
        return list(self._members.get(group_id, {}).values())


class SessionLogGroupBudgetStore:
    """Persists the group ledger + membership to a ``SessionLog``, keyed by a group-anchor session whose
    id is the group id. Budget/membership rebuild by folding ``group_budget_charged`` /
    ``group_member_joined`` events on read (spec §2.4). Durable + replica-spanning when the underlying
    ``SessionLog`` is."""

    def __init__(self, log: "SessionLog") -> None:
        self._log = log

    async def read(self, group_id: str) -> GroupLedger:
        tokens = subagents = 0
        for entry in await self._log.read(group_id):
            if entry.event.get("kind") == "group_budget_charged":
                tokens += entry.event["tokens"]
                subagents += entry.event["subagents"]
        return GroupLedger(tokens, subagents)

    async def charge(self, group_id: str, *, tokens: int = 0, subagents: int = 0) -> None:
        await self._log.append(group_id, {
            "kind": "group_budget_charged",
            "tokens": max(0, tokens),
            "subagents": max(0, subagents),
        })

    async def join(self, group_id: str, member: GroupMember) -> None:
        existing = await self.members(group_id)
        if any(m.session_id == member.session_id for m in existing):
            return
        await self._log.append(group_id, {
            "kind": "group_member_joined",
            "session_id": member.session_id,
            "role": member.role,
        })

    async def members(self, group_id: str) -> list[GroupMember]:
        seen: dict[str, GroupMember] = {}
        for entry in await self._log.read(group_id):
            if entry.event.get("kind") == "group_member_joined":
                sid = entry.event["session_id"]
                seen[sid] = GroupMember(sid, entry.event.get("role"))
        return list(seen.values())


@dataclass
class RunGroup:
    """Binds a runner to a governance domain: a stable group id + the store its members share."""

    id: str
    budget_store: GroupBudgetStore
