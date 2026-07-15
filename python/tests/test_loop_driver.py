"""③ dynamic loop agent — LoopDriver over the kernel pacing trap (python parity with
node/tests/runtime/loop-driver.test.ts)."""

import asyncio
import time

import pytest

from deepstrike import (
    InMemorySessionLog,
    InMemoryGroupBudgetStore,
    LocalExecutionPlane,
    LoopDriver,
    RuntimeOptions,
    RuntimeRunner,
    RuntimeSignal,
    RunGroup,
    SignalGateway,
    fold_loop_state,
    run_loop,
    signal_aware_sleeper,
)
from deepstrike.runtime.loop_driver import LoopSpec
from deepstrike.providers.base import Message
from deepstrike.providers.stream import TextDelta, ToolCallEvent


class ScriptedLoopProvider:
    """Scripted loop provider: each ROUND proposes a pace verb, then files its round report."""

    def __init__(self, script: list[dict]) -> None:
        self._script = script
        self._call = 0
        self._round = 0

    async def complete(self, context, tools, extensions=None):
        return Message(role="assistant", content="done")

    async def stream(self, context, tools, extensions=None, state=None):
        self._call += 1
        # Odd calls: propose pace for the current round; even calls: the strip-tools
        # final report turn the kernel forces after an allowed pace.
        if self._call % 2 == 1:
            step = self._script[min(self._round, len(self._script) - 1)]
            self._round += 1
            args = {"next": step["next"], "reason": f"scripted round {self._round}"}
            if step.get("delay_ms") is not None:
                args["delay_ms"] = step["delay_ms"]
            yield ToolCallEvent(id=f"pace-{self._round}", name="pace", arguments=args)
            return
        yield TextDelta(delta=f"round {self._round} report")


def _make_runner(provider) -> tuple[RuntimeRunner, InMemorySessionLog]:
    session_log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=16_000,
    ))
    return runner, session_log


@pytest.mark.asyncio
async def test_drives_continue_sleep_stop_under_one_session_with_durable_pace_trail():
    provider = ScriptedLoopProvider([
        {"next": "continue"},
        {"next": "sleep", "delay_ms": 5},  # below the 10s floor → kernel clamps + records coercion
        {"next": "stop"},
    ])
    runner, session_log = _make_runner(provider)
    slept: list[int] = []

    async def sleeper(delay_ms, wake_at_ms):
        slept.append(delay_ms)
        return True  # in-process wake, no real timer

    outcome = await run_loop(runner, LoopSpec(
        loop_id="loop-basic",
        goal="iterate the widget until done",
        min_sleep_ms=10_000,
        max_sleep_ms=600_000,
        sleeper=sleeper,
    ))

    assert outcome.stopped is True
    assert outcome.state == "stopped"
    assert outcome.rounds_completed == 3
    assert outcome.last_pace and outcome.last_pace["action"] == "stop"

    # Durable pacing trail: 3 round_started + 3 round_paced on the ONE session id,
    # with the kernel's clamp coercion recorded on round 2.
    events = [e.event for e in await session_log.read("loop-basic")]
    started = [e for e in events if e.get("kind") == "round_started"]
    paced = [e for e in events if e.get("kind") == "round_paced"]
    assert len(started) == 3
    assert [p["action"] for p in paced] == ["continue", "sleep", "stop"]
    assert paced[1]["delay_ms"] == 10_000
    assert "clamped" in paced[1]["coerced_from"]
    assert paced[1]["wake_at_ms"] > 0
    # The in-kernel clamp actually slept the clamped duration (±scheduling slack).
    assert len(slept) == 1
    assert slept[0] > 9_000

    # Transcript continuity: one growing session, multiple run_started/run_terminal pairs.
    assert len([e for e in events if e.get("kind") == "run_terminal"]) == 3


@pytest.mark.asyncio
async def test_resumes_mid_sleep_from_folded_log_and_goes_dormant_on_sleeper_handoff():
    provider = ScriptedLoopProvider([{"next": "sleep", "delay_ms": 60_000}])
    runner, session_log = _make_runner(provider)

    async def decline_sleeper(delay_ms, wake_at_ms):
        return False  # stateless host: hand the wake to an external scheduler

    # Round 1 ends in sleep; the sleeper declines (stateless host) → dormant with wake time.
    first = await run_loop(runner, LoopSpec(
        loop_id="loop-dormant",
        goal="cron tick",
        min_sleep_ms=1_000,
        sleeper=decline_sleeper,
    ))
    assert first.state == "dormant"
    assert first.wake_at_ms > int(time.time() * 1000) - 1_000

    # The fold alone recovers round count + pending wake — zero extra storage.
    folded = fold_loop_state(await session_log.read("loop-dormant"))
    assert folded.rounds_completed == 1
    assert folded.pending_wake_at_ms == first.wake_at_ms


@pytest.mark.asyncio
async def test_verdict_fn_overrides_stop_at_most_k_times_and_feeds_feedback():
    provider = ScriptedLoopProvider([
        {"next": "stop"},
        {"next": "stop"},
        {"next": "stop"},
    ])
    runner, session_log = _make_runner(provider)
    judged: list[int] = []

    def verdict_fn(ctx):
        judged.append(ctx["round"])
        return {"pass": False, "feedback": "tests are still red"}

    outcome = await LoopDriver(runner, LoopSpec(
        loop_id="loop-verdict",
        goal="ship the fix",
        max_verdict_overrides=2,
        verdict_fn=verdict_fn,
    )).run()

    # Two overrides then the third stop stands (K exhausted; judge no longer consulted).
    assert judged == [1, 2]
    assert outcome.stopped is True
    assert outcome.rounds_completed == 3

    events = [e.event for e in await session_log.read("loop-verdict")]
    paced = [e for e in events if e.get("kind") == "round_paced"]
    assert paced[0]["action"] == "continue"
    assert "stop" in paced[0]["coerced_from"]
    # The judge's feedback steers the next round's goal.
    starts = [e for e in events if e.get("kind") == "round_started"]
    assert len(starts) == 3


@pytest.mark.asyncio
async def test_enforces_max_rounds_as_ungrouped_backstop_without_extra_round():
    provider = ScriptedLoopProvider([{"next": "continue"}])
    runner, session_log = _make_runner(provider)

    outcome = await run_loop(runner, LoopSpec(loop_id="loop-cap", goal="spin", max_rounds=2))
    assert outcome.stopped is True
    assert outcome.rounds_completed == 2
    assert "max_rounds" in outcome.last_pace["reason"]
    events = [e.event for e in await session_log.read("loop-cap")]
    assert len([e for e in events if e.get("kind") == "round_started"]) == 2


@pytest.mark.asyncio
async def test_grouped_loop_settles_exactly_one_round_per_vehicle():
    provider = ScriptedLoopProvider([
        {"next": "continue"},
        {"next": "continue"},
        {"next": "stop"},
    ])
    runner, _ = _make_runner(provider)
    store = InMemoryGroupBudgetStore()
    runner.host_options.run_group = RunGroup(id="loop-group", budget_store=store)

    outcome = await run_loop(runner, LoopSpec(
        loop_id="loop-grouped",
        goal="iterate",
        max_rounds=3,
    ))

    assert outcome.rounds_completed == 3
    assert (await store.read("loop-group")).rounds_completed == 3


@pytest.mark.asyncio
async def test_dw5_verdict_override_budget_folds_from_log():
    """DW-5: the verdict override budget folds from the log — a restart grants no fresh overrides."""
    provider = ScriptedLoopProvider([{"next": "stop"}])
    runner, session_log = _make_runner(provider)
    # Simulate a pre-crash trail: 2 rounds whose stops were already overridden by the judge.
    await session_log.append("loop-refold", {"kind": "round_started", "round": 1, "goal": "g"})
    await session_log.append("loop-refold", {
        "kind": "round_paced", "round": 1, "action": "continue",
        "reason": "verdict override 1: tests are still red", "coerced_from": "stop (done)",
    })
    await session_log.append("loop-refold", {"kind": "round_started", "round": 2, "goal": "g"})
    await session_log.append("loop-refold", {
        "kind": "round_paced", "round": 2, "action": "continue",
        "reason": "verdict override 2: still red", "coerced_from": "stop (done)",
    })

    judged: list[int] = []

    def verdict_fn(ctx):
        judged.append(ctx["round"])
        return {"pass": False, "feedback": "no"}

    outcome = await LoopDriver(runner, LoopSpec(
        loop_id="loop-refold",
        goal="ship the fix",
        max_verdict_overrides=2,
        verdict_fn=verdict_fn,
    )).run()
    # Budget exhausted by the folded trail: the judge is never consulted, the stop stands.
    assert judged == []
    assert outcome.stopped is True
    assert outcome.rounds_completed == 3


@pytest.mark.asyncio
async def test_dw6_signal_aware_sleeper_wakes_on_recipient_addressed_signal():
    gateway = SignalGateway()
    sleeper = signal_aware_sleeper(gateway, "loop-wake")
    t0 = time.time()
    sleeping = asyncio.ensure_future(sleeper(60_000, int(t0 * 1000) + 60_000))
    await asyncio.sleep(0)  # let the sleeper register its listener
    # A signal addressed to ANOTHER loop must not wake us.
    gateway.ingest(RuntimeSignal(
        source="custom", signal_type="event", urgency="normal",
        payload={}, recipient="someone-else",
    ))

    async def _wake() -> None:
        await asyncio.sleep(0.01)
        # The completion→wake bridge: a signal addressed to THIS loop ends the sleep immediately.
        gateway.ingest(RuntimeSignal(
            source="custom", signal_type="event", urgency="normal",
            payload={"goal": "wf done"}, recipient="loop-wake",
        ))

    asyncio.ensure_future(_wake())
    woke = await sleeping
    assert woke is True
    assert time.time() - t0 < 5.0
    # The wake signal stays queued for the next round's kernel signal path (visible to the model).
    assert len(gateway._pending) == 2
    gateway.destroy()


@pytest.mark.asyncio
async def test_round_that_never_paces_falls_back_to_kernel_default_action():
    # Provider ends immediately with text — no pace call; goal loop default = stop.
    class SilentProvider:
        async def complete(self, context, tools, extensions=None):
            return Message(role="assistant", content="all done")

        async def stream(self, context, tools, extensions=None, state=None):
            yield TextDelta(delta="all done")

    runner, _ = _make_runner(SilentProvider())
    outcome = await run_loop(runner, LoopSpec(loop_id="loop-nopace", goal="one shot"))
    assert outcome.stopped is True
    assert outcome.rounds_completed == 1
    assert outcome.last_pace["action"] == "stop"
    assert "default_action" in outcome.last_pace["reason"]
