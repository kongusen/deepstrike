"""Canonical-ABI Task 0 — pre-migration stop-the-bleeding fixes for the effect lifecycle.

R-B27: ``evaluate_milestone`` with neither a phase ``verifier`` nor an ``on_milestone_evaluate``
       hook used to ``return`` straight out of the loop, leaving the milestone effect alive in the
       kernel's pending table forever. It must now feed back a conservative resolution
       (``passed=False``) — pending cleared, phase NOT advanced.
R-B28: the main loop's ``if / elif`` chain had no ``else``. An effect kind with no branch at that
       position left ``action`` unreplaced and no event in flight, so
       ``while not runtime.is_terminal()`` spun at 100% CPU forever. It must now terminate the run
       with an explicit error.

Mirrors ``node/tests/effect-hygiene.test.ts`` and ``wasm/tests/effect-hygiene.test.ts``.
"""

import asyncio

import pytest

from deepstrike import (
    MILESTONE_UNVERIFIED_REASON, InMemorySessionLog, LocalExecutionPlane,
    MilestoneContract, MilestonePhase, RuntimeOptions, RuntimeRunner,
)
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import runner as runner_module
from deepstrike.runtime.kernel_step import KernelRunnerAction


class FakeProvider:
    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="done")


def _make_runner(session_log: InMemorySessionLog, **extra):
    return RuntimeRunner(RuntimeOptions(
        provider=FakeProvider(),
        session_log=session_log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=4000,
        max_turns=6,
        **extra,
    ))


@pytest.mark.asyncio
async def test_unverifiable_milestone_effect_is_resolved():
    """R-B27: the run still ends `milestone_pending`, but the effect no longer dangles."""
    session_log = InMemorySessionLog()
    runner = _make_runner(
        session_log,
        milestone_contract=MilestoneContract(phases=[
            MilestonePhase(id="phase1", criteria=["must complete"]),
        ]),
        milestone_policy="require_verifier",
    )

    events = []
    async for evt in runner.run(goal="test", session_id="ms_leak"):
        events.append(evt)

    done = [e for e in events if getattr(e, "type", None) == "done"]
    assert len(done) == 1
    assert done[0].status == "milestone_pending"

    logged = [entry.event for entry in await session_log.read("ms_leak")]
    # The kernel only emits `milestone_blocked` from `handle_milestone_result`, which is also
    # where the pending effect is removed — its presence proves the resolution was accepted.
    blocked = next((e for e in logged if e.get("kind") == "milestone_blocked"), None)
    assert blocked is not None
    assert blocked["phase_id"] == "phase1"
    assert blocked["reason"] == MILESTONE_UNVERIFIED_REASON

    # Fail-closed: the phase did NOT advance and no capability was unlocked.
    assert not any(e.get("kind") == "milestone_advanced" for e in logged)


@pytest.mark.asyncio
async def test_unhandled_effect_terminates_instead_of_busy_waiting(monkeypatch):
    """R-B28: an effect with no main-loop branch fails closed rather than pinning a core.

    Wall-clock is the busy-wait detector: before the ``else`` backstop this run never returned —
    and because every await in the spin path resolves synchronously, it starved the whole event
    loop (even the ``asyncio.wait_for`` timer below never got a chance to fire).
    """
    session_log = InMemorySessionLog()
    runner = _make_runner(session_log)

    # Forge the situation the audit describes: an effect that IS part of the action union but is
    # only ever driven inside the workflow driver arrives at the main-loop position. Swapping the
    # mapped action (not the kernel's own step) keeps the kernel transaction chain intact while
    # reproducing exactly what the loop sees.
    original = runner_module.action_host
    forged = []

    async def forging_action_host(runtime, pending, event):
        action = await original(runtime, pending, event)
        if not forged and action.kind == "call_provider":
            forged.append(True)
            return KernelRunnerAction(
                kind="preempt_sub_agents",
                effect_id=action.effect_id,
                agent_ids=["ghost-agent"],
                reason="test",
            )
        return action

    monkeypatch.setattr(runner_module, "action_host", forging_action_host)

    async def drain():
        return [evt async for evt in runner.run(goal="test", session_id="unhandled_effect")]

    events = await asyncio.wait_for(drain(), timeout=15)

    assert forged
    errors = [e for e in events if getattr(e, "type", None) == "error"]
    assert len(errors) == 1
    assert "unhandled kernel effect preempt_sub_agents" in errors[0].message

    done = [e for e in events if getattr(e, "type", None) == "done"]
    assert len(done) == 1
    assert done[0].status == "error"

    logged = [entry.event for entry in await session_log.read("unhandled_effect")]
    terminal = next((e for e in logged if e.get("kind") == "run_terminal"), None)
    assert terminal is not None
    assert terminal["reason"] == "error"
