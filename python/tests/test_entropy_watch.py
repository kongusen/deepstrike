"""Session entropy — the kernel-side measurement behind a host "heartbeat entropy watch":
one ``entropy_sample`` stream event per completed turn (unconditional), plus the opt-in
``entropy_watch`` threshold alert (``entropy_alert``), both mirrored into the session log.
``runner.latest_entropy()`` is the pull companion for supervisors outside the stream."""

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import EntropyAlertEvent, EntropySampleEvent, TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


class LoopingProvider:
    """Repeats the identical failing tool call ``tool_turns`` times, then finishes."""

    def __init__(self, tool_turns: int) -> None:
        self.tool_turns = tool_turns
        self.calls: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls.append(context)
        if len(self.calls) <= self.tool_turns:
            yield ToolCallEvent(id=f"call_{len(self.calls)}", name="poke", arguments={"same": True})
            return
        yield TextDelta(delta="done")


def _failing_poke():
    @tool
    def poke(same: bool = True) -> str:
        """Poke the thing."""
        raise RuntimeError("still broken")

    return poke


def _runner(provider, poke, **opts) -> tuple[RuntimeRunner, InMemorySessionLog]:
    session_log = InMemorySessionLog()
    plane = LocalExecutionPlane().register(poke)
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=plane,
        max_tokens=2048,
        max_turns=10,
        **opts,
    ))
    return runner, session_log


@pytest.mark.asyncio
async def test_entropy_sample_streams_every_completed_turn_and_latest_entropy():
    runner, _log = _runner(
        LoopingProvider(3), _failing_poke(),
        repeat_fuse=False,  # keep the loop alive long enough to observe samples
    )
    samples: list[EntropySampleEvent] = []
    async for event in runner.run(goal="poke it"):
        if isinstance(event, EntropySampleEvent):
            samples.append(event)

    assert len(samples) >= 3
    last = samples[-1].sample
    assert last.score_version == 1
    assert last.failure_rate == pytest.approx(1.0)  # every poke raised
    assert last.repeat_pressure == 0.0  # fuse off ⇒ repeat axis honestly reads 0
    assert last.window_turns == len(samples)
    assert runner.latest_entropy() == last


@pytest.mark.asyncio
async def test_entropy_alert_is_optin_and_lands_in_session_log():
    # Watch OFF: a disordered run never alerts.
    runner, _log = _runner(LoopingProvider(3), _failing_poke())
    async for event in runner.run(goal="poke it"):
        assert not isinstance(event, EntropyAlertEvent)

    # Watch ON with a floor threshold: alerts exactly once while the score stays hot
    # (hysteresis disarms re-fires), and both event kinds reach the session log.
    runner, session_log = _runner(
        LoopingProvider(4), _failing_poke(),
        entropy_watch={"threshold": 0.1, "cooldown_turns": 0},
    )
    alerts: list[EntropyAlertEvent] = []
    async for event in runner.run(session_id="entropy-on", goal="poke it"):
        if isinstance(event, EntropyAlertEvent):
            alerts.append(event)

    assert len(alerts) == 1
    assert alerts[0].threshold == pytest.approx(0.1)
    assert alerts[0].score > 0.1

    kinds = [entry.event.get("kind") for entry in await session_log.read("entropy-on")]
    assert "entropy_sample" in kinds
    assert "entropy_alert" in kinds


@pytest.mark.asyncio
async def test_notify_model_feeds_a_durable_entropy_directive():
    provider = LoopingProvider(3)
    runner, _log = _runner(
        provider, _failing_poke(),
        entropy_watch={"threshold": 0.1, "cooldown_turns": 0, "notify_model": True},
    )
    async for _ in runner.run(goal="poke it"):
        pass

    def rendered(ctx: RenderedContext) -> str:
        parts = [getattr(ctx, "system_text", None)]
        state_turn = getattr(ctx, "state_turn", None)
        if state_turn is not None:
            parts.append(getattr(state_turn, "content", None))
        parts.extend(getattr(m, "content", None) for m in ctx.turns)
        return "\n".join(p for p in parts if p)

    assert any("[entropy]" in rendered(c) for c in provider.calls)
