"""L2 (ReactiveSession) — the user-facing primitive for "N peer agents over a shared event stream".

Composes the lower layers so teams don't hand-roll the pattern:
  - L1 ``RunGroup``      — shared governance domain (cumulative budget + lineage) across the personas.
  - L0 ``SignalGateway`` — recipient-routed signals (targeted ``interrupt`` / ``broadcast``).
  - ``EventStream``      — the shared blackboard (pluggable; default in-memory).
  - ``TurnPolicy``       — who reacts to each event (the one caller-customizable seam).

Stateless-friendly: ``emit`` can run inside an HTTP handler; each persona's turn is a normal
``run(session_id=...)`` whose continuity comes from its ``SessionLog``, and ``resume`` rebuilds the
peer set from the persisted ``RunGroup`` membership — no hot in-process loop required.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from deepstrike.runtime.event_stream import BlackboardEvent, EventStream, EventViewer, InMemoryEventStream, is_visible_to
from deepstrike.runtime.run_group import GroupMember, RunGroup
from deepstrike.runtime.runner import collect_text
from deepstrike.runtime.turn_policy import PeerView, TurnPolicy
from deepstrike.signals.gateway import SignalGateway
from deepstrike.signals.types import RuntimeSignal
from deepstrike.tools import tool

if TYPE_CHECKING:
    from deepstrike.runtime.runner import RuntimeRunner


@dataclass
class ReactorContext:
    """Context for one reactive turn. ``runner`` is wired to the shared RunGroup / signal gateway /
    blackboard, so whatever the turn body drives (e.g. ``runner.run_workflow``) stays under one
    governance domain."""
    persona_id: str
    goal: str
    event: BlackboardEvent
    runner: "RuntimeRunner"


# A turn body: given the context, return the persona's reaction text. Override to make a peer's turn a
# different orchestration form (DAG via ``run_workflow``, nested ensemble, …). Default = one ``run()``.
ReactorTurn = Callable[["ReactorContext"], Awaitable[str]]


@dataclass
class ReactivePeerSpec:
    goal: str | None = None
    role: str | None = None
    channels: list[str] = field(default_factory=list)
    # Turn-body seam (the DAG-in-Peer enabler): override this persona's reaction body. Defaults to the
    # session ``react_with``, then to a single ``run()`` agent turn.
    react: ReactorTurn | None = None


@dataclass
class Reaction:
    persona_id: str
    output: str


# make_runner(persona_id, shared) -> RuntimeRunner; shared = {run_group, signal_source, event_stream}
MakeRunner = Callable[[str, dict[str, Any]], "RuntimeRunner"]


class ReactiveSession:
    def __init__(
        self,
        *,
        run_group: RunGroup,
        turn_policy: TurnPolicy,
        make_runner: MakeRunner,
        event_stream: EventStream | None = None,
        signal_gateway: SignalGateway | None = None,
        goal_for: Callable[[str, BlackboardEvent], str] | None = None,
        react_with: ReactorTurn | None = None,
    ) -> None:
        self._run_group = run_group
        self._turn_policy = turn_policy
        self._make_runner = make_runner
        self._event_stream = event_stream or InMemoryEventStream()
        self._gateway = signal_gateway or SignalGateway()
        self._goal_for = goal_for
        self._react_with = react_with
        self._peer_specs: dict[str, ReactivePeerSpec] = {}
        self._runners: dict[str, "RuntimeRunner"] = {}
        self._policy_state: dict[str, Any] = {}

    def add_peer(self, persona_id: str, *, goal: str | None = None, role: str | None = None,
                 channels: list[str] | None = None, react: ReactorTurn | None = None) -> None:
        self._peer_specs[persona_id] = ReactivePeerSpec(goal, role, channels or [], react)

    def peers(self) -> list[str]:
        return list(self._peer_specs.keys())

    def blackboard(self) -> EventStream:
        return self._event_stream

    async def emit(self, payload: Any, *, source: str | None = None,
                   channel: str | None = None, audience: list[str] | None = None) -> list[Reaction]:
        bb = await self._event_stream.append(payload, source=source, channel=channel, audience=audience)
        # Record lineage for all registered peers (idempotent).
        for pid, spec in self._peer_specs.items():
            await self._run_group.budget_store.join(self._run_group.id, GroupMember(pid, spec.role))

        candidates = [
            PeerView(pid, spec.role, spec.channels)
            for pid, spec in self._peer_specs.items()
            if is_visible_to(bb, EventViewer(pid, spec.channels))
        ]
        chosen = self._turn_policy(bb, candidates, self._policy_state)
        if hasattr(chosen, "__await__"):
            chosen = await chosen
        eligible = {p.persona_id for p in candidates}

        reactions: list[Reaction] = []
        for pid in chosen:
            if pid in eligible:
                reactions.append(Reaction(pid, await self._drive_turn(pid, bb)))
        return reactions

    async def interrupt(self, persona_id: str, *, payload: dict | None = None) -> None:
        self._gateway.ingest(RuntimeSignal(
            kind="interrupt", payload=payload or {}, source="gateway",
            signal_type="alert", urgency="critical", recipient=persona_id,
        ))

    async def broadcast(self, *, payload: dict | None = None) -> None:
        self._gateway.ingest(RuntimeSignal(kind="external", payload=payload or {}, source="gateway"))

    def _get_runner(self, persona_id: str) -> "RuntimeRunner":
        runner = self._runners.get(persona_id)
        if runner is None:
            runner = self._make_runner(persona_id, {
                "run_group": self._run_group,
                "signal_source": self._gateway,
                "event_stream": self._event_stream,
            })
            self._runners[persona_id] = runner
        return runner

    async def _drive_turn(self, persona_id: str, event: BlackboardEvent) -> str:
        runner = self._get_runner(persona_id)
        goal = (
            (self._goal_for(persona_id, event) if self._goal_for else None)
            or self._peer_specs[persona_id].goal
            or "React to the latest events on the shared blackboard."
        )
        # Turn-body seam (DAG-in-Peer): per-peer ``react`` wins, then session ``react_with``, else the
        # default single agent turn. The body's runner inherits the shared RunGroup.
        react = self._peer_specs[persona_id].react or self._react_with
        if react is not None:
            return await react(ReactorContext(persona_id, goal, event, runner))
        return await collect_text(runner.run(session_id=persona_id, goal=goal))

    @staticmethod
    async def resume(
        *, run_group: RunGroup, turn_policy: TurnPolicy, make_runner: MakeRunner,
        event_stream: EventStream | None = None,
        peer_specs: dict[str, ReactivePeerSpec] | None = None,
    ) -> "ReactiveSession":
        session = ReactiveSession(
            run_group=run_group, turn_policy=turn_policy,
            make_runner=make_runner, event_stream=event_stream,
        )
        for member in await run_group.budget_store.members(run_group.id):
            spec = (peer_specs or {}).get(member.session_id) or ReactivePeerSpec(role=member.role)
            session._peer_specs[member.session_id] = spec
        return session


def read_recent_tool(event_stream: EventStream, viewer: EventViewer):
    """A ``read_recent`` tool a persona uses to read the shared blackboard, scoped to what it may see."""

    async def read_recent(since_seq: int = -1, channel: str = "") -> str:
        """Read recent blackboard events visible to you (optionally a single channel you subscribe to)."""
        events = await event_stream.read_since(since_seq, viewer)
        if channel:
            events = [e for e in events if e.channel == channel]
        return json.dumps([
            {"seq": e.seq, "source": e.source, "channel": e.channel, "payload": e.payload}
            for e in events
        ])

    return tool(read_recent)
