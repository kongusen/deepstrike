"""L2 (TurnPolicy) — who reacts to a blackboard event.

The one caller-customizable seam of a ``ReactiveSession``; the framework supplies a spanning default
set (addressing / delegated / deterministic-cyclic) plus combinators, so teams compose rather than
hand-roll turn-taking. Stateful policies (e.g. ``round_robin``) keep their cursor in the ``state``
dict, which a ``ReactiveSession`` carries across turns.

A TurnPolicy is ``async (event, peers, state) -> list[str]`` (sync callables are also accepted).
"""

from __future__ import annotations

import inspect
import json
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable

from deepstrike.runtime.event_stream import BlackboardEvent


@dataclass
class PeerView:
    persona_id: str
    role: str | None = None
    channels: list[str] = field(default_factory=list)


TurnPolicy = Callable[[BlackboardEvent, list[PeerView], dict[str, Any]], "list[str] | Awaitable[list[str]]"]


async def _call(policy: TurnPolicy, event, peers, state) -> list[str]:
    out = policy(event, peers, state)
    return await out if inspect.isawaitable(out) else out


def react_by_mention() -> TurnPolicy:
    """Addressing: react iff the event names the peer (its ``audience``, or its id/role in payload)."""

    def policy(event: BlackboardEvent, peers: list[PeerView], state: dict[str, Any]) -> list[str]:
        hay = event.payload if isinstance(event.payload, str) else json.dumps(event.payload)
        chosen = []
        for p in peers:
            if event.audience and p.persona_id in event.audience:
                chosen.append(p.persona_id)
            elif p.persona_id in hay or (p.role is not None and p.role in hay):
                chosen.append(p.persona_id)
        return chosen

    return policy


def director_driven(
    director_id: str,
    select: Callable[[BlackboardEvent, list[PeerView]], "list[str] | Awaitable[list[str]]"],
) -> TurnPolicy:
    """Delegated: a designated persona (or fn, possibly an LLM call) decides who reacts."""

    async def policy(event: BlackboardEvent, peers: list[PeerView], state: dict[str, Any]) -> list[str]:
        chosen = select(event, peers)
        chosen = await chosen if inspect.isawaitable(chosen) else chosen
        valid = {p.persona_id for p in peers}
        return [c for c in chosen if c in valid and c != director_id]

    return policy


def round_robin() -> TurnPolicy:
    """Deterministic: cycle through peers in order, one per event. Cursor persisted in ``state``."""

    def policy(event: BlackboardEvent, peers: list[PeerView], state: dict[str, Any]) -> list[str]:
        if not peers:
            return []
        cursor = state.get("rr_cursor", 0)
        state["rr_cursor"] = cursor + 1
        return [peers[cursor % len(peers)].persona_id]

    return policy


def first_non_empty(*policies: TurnPolicy) -> TurnPolicy:
    """Combinator: first policy that selects a non-empty set wins."""

    async def policy(event: BlackboardEvent, peers: list[PeerView], state: dict[str, Any]) -> list[str]:
        for p in policies:
            chosen = await _call(p, event, peers, state)
            if chosen:
                return chosen
        return []

    return policy


def union(*policies: TurnPolicy) -> TurnPolicy:
    """Combinator: union of all policies' selections (deduped, order-stable)."""

    async def policy(event: BlackboardEvent, peers: list[PeerView], state: dict[str, Any]) -> list[str]:
        seen: dict[str, None] = {}
        for p in policies:
            for pid in await _call(p, event, peers, state):
                seen[pid] = None
        return list(seen.keys())

    return policy
