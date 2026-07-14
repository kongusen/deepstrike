"""L2 — EventStream visibility, TurnPolicy default set, and ReactiveSession orchestration (spec §6)."""

import pytest

from deepstrike import (
    RuntimeRunner, RuntimeOptions, InMemorySessionLog, LocalExecutionPlane,
    RunGroup, InMemoryGroupBudgetStore,
    InMemoryEventStream, EventViewer, BlackboardEvent, is_visible_to,
    PeerView, react_by_mention, director_driven, round_robin, first_non_empty,
    InMemoryReactionCheckpointStore, ReactiveSession, read_recent_tool,
)
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta


# ── EventStream visibility ──────────────────────────────────────────────────
@pytest.mark.asyncio
async def test_event_stream_visibility():
    s = InMemoryEventStream()
    await s.append("to-all")
    await s.append("to-coach", audience=["coach", "learner"])
    await s.append("ch-a", channel="a")

    coach = await s.read_since(-1, EventViewer("coach", []))
    assert [e.payload for e in coach] == ["to-all", "to-coach"]

    role = await s.read_since(-1, EventViewer("role", ["a"]))
    assert [e.payload for e in role] == ["to-all", "ch-a"]

    assert len(await s.read_since(-1)) == 3


def test_is_visible_to():
    assert is_visible_to(BlackboardEvent(0, "x"), EventViewer("x"))
    assert not is_visible_to(BlackboardEvent(0, "x", audience=["y"]), EventViewer("x"))
    assert is_visible_to(BlackboardEvent(0, "x", channel="c"), EventViewer("x", ["c"]))


# ── TurnPolicy default set ──────────────────────────────────────────────────
_PEERS = [PeerView("director", "director"), PeerView("alice", "buyer"), PeerView("bob", "seller")]


def _ev(payload, audience=None):
    return BlackboardEvent(0, payload, audience=audience)


@pytest.mark.asyncio
async def test_react_by_mention():
    assert react_by_mention()(_ev("hey alice"), _PEERS, {}) == ["alice"]
    assert react_by_mention()(_ev("x", ["bob"]), _PEERS, {}) == ["bob"]


@pytest.mark.asyncio
async def test_director_driven_excludes_director():
    policy = director_driven("director", lambda e, p: ["alice", "director"])
    assert await policy(_ev("?"), _PEERS, {}) == ["alice"]


@pytest.mark.asyncio
async def test_round_robin_cycles():
    state, policy, seq = {}, round_robin(), []
    for i in range(4):
        seq.append(policy(_ev(i), _PEERS, state)[0])
    assert seq == ["director", "alice", "bob", "director"]


@pytest.mark.asyncio
async def test_first_non_empty_fallback():
    policy = first_non_empty(react_by_mention(), director_driven("director", lambda e, p: ["bob"]))
    assert await policy(_ev("nobody named"), _PEERS, {}) == ["bob"]
    assert await policy(_ev("alice here"), _PEERS, {}) == ["alice"]


# ── ReactiveSession orchestration ───────────────────────────────────────────
class _TextProvider:
    def __init__(self, persona_id):
        self._pid = persona_id

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content=f"{self._pid}-ack")

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta=f"{self._pid}-ack")


def _make_session(turn_policy):
    store = InMemoryGroupBudgetStore()
    run_group = RunGroup(id="scenario", budget_store=store)

    def make_runner(persona_id, shared):
        plane = LocalExecutionPlane()
        plane.register(read_recent_tool(shared["event_stream"], EventViewer(persona_id)))
        return RuntimeRunner(RuntimeOptions(
            provider=_TextProvider(persona_id),
            session_log=InMemorySessionLog(),
            execution_plane=plane,
            max_tokens=4096,
            agent_id=persona_id,
            run_group=shared["run_group"],
            signal_source=shared["signal_source"],
        ))

    session = ReactiveSession(run_group=run_group, turn_policy=turn_policy, make_runner=make_runner)
    return session, store


@pytest.mark.asyncio
async def test_duplicate_idempotent_event_returns_checkpoint_without_rerunning():
    event_stream = InMemoryEventStream()
    checkpoint_store = InMemoryReactionCheckpointStore()
    run_group = RunGroup(id="retry", budget_store=InMemoryGroupBudgetStore())
    calls = 0

    def build():
        session = ReactiveSession(
            run_group=run_group,
            turn_policy=react_by_mention(),
            event_stream=event_stream,
            checkpoint_store=checkpoint_store,
            make_runner=lambda _persona_id, _shared: object(),
        )

        async def react(_ctx):
            nonlocal calls
            calls += 1
            return f"alice-{calls}"

        session.add_peer("alice", react=react)
        return session

    first = await build().emit("alice", idempotency_key="event-1")
    retried = await build().emit("alice", idempotency_key="event-1")

    assert retried == first
    assert calls == 1
    assert len(await event_stream.read_since(-1)) == 1


@pytest.mark.asyncio
async def test_partial_failure_reuses_plan_and_completed_peer_outputs():
    event_stream = InMemoryEventStream()
    checkpoint_store = InMemoryReactionCheckpointStore()
    run_group = RunGroup(id="partial", budget_store=InMemoryGroupBudgetStore())
    calls = {"alice": 0, "bob": 0}

    async def choose(_event, _peers, _state):
        return ["alice", "bob"]

    session = ReactiveSession(
        run_group=run_group,
        turn_policy=choose,
        event_stream=event_stream,
        checkpoint_store=checkpoint_store,
        make_runner=lambda _persona_id, _shared: object(),
    )

    async def alice(_ctx):
        calls["alice"] += 1
        return f"alice-{calls['alice']}"

    async def bob(_ctx):
        calls["bob"] += 1
        if calls["bob"] == 1:
            raise RuntimeError("temporary failure")
        return f"bob-{calls['bob']}"

    session.add_peer("alice", react=alice)
    session.add_peer("bob", react=bob)

    with pytest.raises(RuntimeError, match="temporary failure"):
        await session.emit("go", idempotency_key="event-2")
    retried = await session.emit("go", idempotency_key="event-2")

    assert [(r.persona_id, r.output) for r in retried] == [
        ("alice", "alice-1"), ("bob", "bob-2"),
    ]
    assert calls == {"alice": 1, "bob": 2}


@pytest.mark.asyncio
async def test_emit_drives_selected_peers_under_shared_governance():
    session, store = _make_session(react_by_mention())
    session.add_peer("alice", role="buyer")
    session.add_peer("bob", role="seller")

    reactions = await session.emit("alice, your move", source="director")
    assert [r.persona_id for r in reactions] == ["alice"]
    assert "alice-ack" in reactions[0].output

    members = await store.members("scenario")
    assert sorted(m.session_id for m in members) == ["alice", "bob"]
    assert (await store.read("scenario")).tokens_spent > 0


@pytest.mark.asyncio
async def test_react_seam_overrides_turn_body():
    """Gap-b: a peer's turn body can be overridden via `react` (the DAG-in-Peer seam). The default is a
    single run(); a peer with `react` runs that callback instead, with a runner wired to the group."""
    session, store = _make_session(react_by_mention())

    async def custom(ctx):
        assert ctx.runner is not None and ctx.event is not None
        return f"custom:{ctx.persona_id}"

    session.add_peer("alice", role="buyer", react=custom)
    session.add_peer("bob", role="seller")  # default body (run())

    reactions = await session.emit("alice, your move", source="director")
    assert reactions[0].persona_id == "alice"
    assert reactions[0].output == "custom:alice"  # seam routed to the override, not run()


@pytest.mark.asyncio
async def test_visibility_gates_reactions():
    session, _ = _make_session(round_robin())
    session.add_peer("coach", channels=[])
    session.add_peer("role", channels=["a"])
    reactions = await session.emit("scene", channel="a")
    assert [r.persona_id for r in reactions] == ["role"]


@pytest.mark.asyncio
async def test_resume_rebuilds_peers_from_membership():
    session, store = _make_session(react_by_mention())
    session.add_peer("director", role="director")
    session.add_peer("npc", role="seller")
    await session.emit("kick things off, director and npc")  # joins members

    def make_runner(persona_id, shared):
        return RuntimeRunner(RuntimeOptions(
            provider=_TextProvider(persona_id), session_log=InMemorySessionLog(),
            execution_plane=LocalExecutionPlane(), max_tokens=4096,
            run_group=shared["run_group"], signal_source=shared["signal_source"],
        ))

    resumed = await ReactiveSession.resume(
        run_group=RunGroup(id="scenario", budget_store=store),
        turn_policy=react_by_mention(), make_runner=make_runner,
    )
    assert sorted(resumed.peers()) == ["director", "npc"]
