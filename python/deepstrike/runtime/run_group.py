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


class InMemoryGroupBudgetStore:
    """Process-local default store. One ledger + member set per group id."""

    def __init__(self) -> None:
        self._ledgers: dict[str, GroupLedger] = {}
        self._members: dict[str, dict[str, GroupMember]] = {}

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


class SessionLogGroupBudgetStore:
    """Persists the group ledger + membership to a ``SessionLog``, keyed by a group-anchor session whose
    id is the group id. Budget/membership rebuild by folding ``group_budget_charged`` /
    ``group_member_joined`` events on read (spec §2.4). Durable + replica-spanning when the underlying
    ``SessionLog`` is."""

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


@dataclass
class RunGroup:
    """Binds a runner to a governance domain: a stable group id + the store its members share."""

    id: str
    budget_store: GroupBudgetStore
