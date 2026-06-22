"""L2 (Blackboard) — a shared, append-only event stream that N peer agent sessions observe.

Pluggable storage seam (like ``SessionLog``): the default ``InMemoryEventStream`` is process-local;
back it with Postgres/Redis to span replicas/restarts. Events are shared by default; optional
``channel``/``audience`` tags scope an event to a subset of personas, enforced at the framework
boundary (``read_since(seq, viewer)`` + the ``read_recent`` tool) — context isolation, not convention.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Protocol, runtime_checkable


@dataclass
class BlackboardEvent:
    """One entry on the shared blackboard. ``channel``/``audience`` are optional visibility scoping."""

    seq: int
    payload: Any
    source: str | None = None
    channel: str | None = None
    audience: list[str] | None = None


@dataclass
class EventViewer:
    """A reader's identity for visibility filtering."""

    persona_id: str
    channels: list[str] = field(default_factory=list)


def is_visible_to(event: BlackboardEvent, viewer: EventViewer) -> bool:
    """Default full-share visibility rule (spec §6.1)."""
    if event.audience is None and event.channel is None:
        return True
    if event.audience is not None and viewer.persona_id in event.audience:
        return True
    if event.channel is not None and event.channel in viewer.channels:
        return True
    return False


@runtime_checkable
class EventStream(Protocol):
    async def append(
        self, payload: Any, *, source: str | None = None,
        channel: str | None = None, audience: list[str] | None = None,
    ) -> BlackboardEvent: ...

    async def read_since(self, seq: int, viewer: EventViewer | None = None) -> list[BlackboardEvent]: ...

    def subscribe(self, cb: Callable[[BlackboardEvent], None]) -> Callable[[], None]: ...


class InMemoryEventStream:
    """Process-local default blackboard."""

    def __init__(self) -> None:
        self._events: list[BlackboardEvent] = []
        self._listeners: set[Callable[[BlackboardEvent], None]] = set()

    async def append(
        self, payload: Any, *, source: str | None = None,
        channel: str | None = None, audience: list[str] | None = None,
    ) -> BlackboardEvent:
        ev = BlackboardEvent(len(self._events), payload, source, channel, audience)
        self._events.append(ev)
        for listener in list(self._listeners):
            listener(ev)
        return ev

    async def read_since(self, seq: int, viewer: EventViewer | None = None) -> list[BlackboardEvent]:
        after = [e for e in self._events if e.seq > seq]
        return after if viewer is None else [e for e in after if is_visible_to(e, viewer)]

    def subscribe(self, cb: Callable[[BlackboardEvent], None]) -> Callable[[], None]:
        self._listeners.add(cb)
        return lambda: self._listeners.discard(cb)
