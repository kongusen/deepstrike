"""③ Dynamic loop-agent engineering system — the SDK driver.

A loop agent is NOT a new execution engine:
- a ROUND is exactly one bounded ``RuntimeRunner.run()`` (compaction, RepeatFuse,
  criteria gate, and budget verdicts all apply per round for free);
- CONTINUITY is the session log replayed under ONE stable session_id;
- LIFETIME GOVERNANCE is the RunGroup the rounds are members of;
- the only new decision — what happens AFTER a round — is the model-proposed,
  kernel-adjudicated ``pace`` verb (see the kernel pacing trap). The kernel never
  sleeps; all timers and judge calls live here, in SDK I/O land.

Durable pacing: every round appends ``round_started`` / ``round_paced`` to the loop's
session log, so resume-style recovery is a fold over the log (the
``SessionLogGroupBudgetStore`` pattern) — zero new storage. A stateless host reads
``wake_at_ms`` from the fold and re-arms via its own cron/queue; an in-process host
lets ``run()`` sleep inline.
"""

from __future__ import annotations

import asyncio
import inspect
import time
from dataclasses import dataclass, replace as _dc_replace
from typing import TYPE_CHECKING, Any, Awaitable, Callable

if TYPE_CHECKING:
  from deepstrike.runtime.runner import RuntimeRunner


def _now_ms() -> int:
  return int(time.time() * 1000)


@dataclass
class LoopSpec:
  # Stable loop id = the ONE session id every round replays (transcript continuity).
  loop_id: str
  goal: str
  criteria: list[str] | None = None
  # Hard round cap; the kernel coerces continue/sleep to stop at the cap.
  max_rounds: int | None = None
  # Sleep clamp bounds (ms), enforced in-kernel.
  min_sleep_ms: int | None = None
  max_sleep_ms: int | None = None
  # "stop" (goal loop, default) | "sleep" (cron loop) when a round never calls pace.
  default_action: str | None = None
  # Cross-round done-gate: judges a stop proposal; a failing verdict overrides stop→continue at
  # most ``max_verdict_overrides`` times, its feedback becoming the next round's steering note.
  # The in-kernel O4 criteria gate is the per-round rung of the same ladder — this is the
  # cross-round rung. Called with {"loop_id", "round", "reason"}; returns
  # {"pass": bool, "feedback"?: str} (sync or async).
  verdict_fn: Callable[[dict[str, Any]], Awaitable[dict[str, Any]] | dict[str, Any]] | None = None
  max_verdict_overrides: int | None = None
  # Sleep implementation (injectable for tests / stateless hosts): (delay_ms, wake_at_ms) -> bool.
  # Return False to hand the wake to an external scheduler and end ``run()`` dormant.
  # Default: asyncio.sleep.
  sleeper: Callable[[int, int], Awaitable[bool]] | None = None
  # Per-round event tap (streaming passthrough): (round, event) -> None.
  on_event: Callable[[int, Any], None] | None = None


@dataclass
class LoopOutcome:
  loop_id: str
  rounds_completed: int
  stopped: bool
  # "stopped" | "dormant" (sleeper handed off to an external scheduler)
  state: str
  last_pace: dict[str, Any] | None = None
  last_status: str | None = None
  # Absolute wake time when dormant.
  wake_at_ms: int | None = None


@dataclass
class FoldedLoopState:
  rounds_completed: int = 0
  pending_wake_at_ms: int | None = None
  last_pace: dict[str, Any] | None = None
  overrides_used: int = 0


def fold_loop_state(events: list[Any]) -> FoldedLoopState:
  """Fold the loop's session log into resumable pacing state — zero new storage. DW-5: the judge's
  override budget folds too, so a crash/restart can't grant the verdict_fn fresh overrides."""
  state = FoldedLoopState()
  for entry in events:
    event = entry.event if hasattr(entry, "event") else entry
    if event.get("kind") == "round_paced":
      state.rounds_completed = max(state.rounds_completed, int(event.get("round") or 0))
      state.last_pace = {"action": event.get("action"), "reason": event.get("reason")}
      state.pending_wake_at_ms = event.get("wake_at_ms") if event.get("action") == "sleep" else None
      if str(event.get("reason") or "").startswith("verdict override"):
        state.overrides_used += 1
  return state


def signal_aware_sleeper(gateway: Any, loop_id: str) -> Callable[[int, int], Awaitable[bool]]:
  """DW-6 completion→wake bridge, composed from two existing seams (zero new mechanism): a
  ``sleeper`` that races the timer against an L0 recipient-addressed signal on the shared gateway.
  Ingest a signal with ``recipient=loop_id`` (a subagent/workflow completion, a webhook) and the
  sleeping loop wakes into its next round immediately — where the SAME queued signal then reaches
  the model through the kernel's normal signal path, so the wake reason is visible in-round."""

  async def _sleep(delay_ms: int, wake_at_ms: int) -> bool:  # noqa: ARG001 — wake_at_ms is part of the sleeper contract
    loop = asyncio.get_running_loop()
    woke: asyncio.Future[bool] = loop.create_future()

    def _listener(sig: Any) -> None:
      if getattr(sig, "recipient", None) == loop_id and not woke.done():
        woke.set_result(True)

    unsubscribe = gateway.on_signal(_listener)
    timer = asyncio.ensure_future(asyncio.sleep(max(0, delay_ms) / 1000.0))
    try:
      await asyncio.wait({timer, woke}, return_when=asyncio.FIRST_COMPLETED)
    finally:
      timer.cancel()
      if not woke.done():
        woke.cancel()
      unsubscribe()
    return True

  return _sleep


class LoopDriver:
  def __init__(self, runner: "RuntimeRunner", spec: LoopSpec) -> None:
    self._runner = runner
    self._spec = spec
    self._overrides_used = 0

  async def run(self) -> LoopOutcome:
    """Drive rounds until the loop stops or goes dormant. Resumable by construction: the round
    count and any pending wake are folded from the session log, so calling ``run()`` again after
    a crash / on a stateless host continues in place."""
    from deepstrike.providers.stream import DoneEvent
    from deepstrike.types.agent import AgentIdentity, AgentRunSpec

    spec = self._spec
    loop_id = spec.loop_id
    log = self._runner.host_options.session_log

    # Resume: fold prior rounds + pending wake + the judge's used overrides from the transcript
    # (DW-5: a crash/restart must not refill the verdict_fn's override budget).
    prior = fold_loop_state(await log.read(loop_id))
    round_no = prior.rounds_completed
    self._overrides_used = max(self._overrides_used, prior.overrides_used)
    if prior.pending_wake_at_ms is not None:
      remaining = prior.pending_wake_at_ms - _now_ms()
      if remaining > 0:
        slept = await self._sleep(remaining, prior.pending_wake_at_ms)
        if not slept:
          return LoopOutcome(
            loop_id=loop_id, rounds_completed=round_no, stopped=False,
            state="dormant", wake_at_ms=prior.pending_wake_at_ms,
          )

    feedback: str | None = None
    while True:
      round_no += 1
      # Driver-side round-cap backstop: with a RunGroup, the kernel trap coerces via the seeded
      # ledger; without one, this is the only max_rounds enforcement point.
      if spec.max_rounds is not None and round_no > spec.max_rounds:
        return LoopOutcome(
          loop_id=loop_id, rounds_completed=round_no - 1, stopped=True, state="stopped",
          last_pace={"action": "stop", "reason": f"max_rounds={spec.max_rounds} exhausted"},
        )
      await log.append(loop_id, {"kind": "round_started", "round": round_no, "goal": spec.goal})

      goal = (
        f"{spec.goal}\n\n[LOOP FEEDBACK round {round_no - 1}] {feedback}"
        if feedback
        else spec.goal
      )
      feedback = None

      pace: dict[str, Any] | None = None
      status: str | None = None
      # ONE round = one bounded kernel run under the stable loop session id. The kernel's pacing
      # trap adjudicates the model's pace proposal; we consume it from the done event.
      # run_spec.loop_round arms the trap + the pace tool.
      prior_run_spec = self._runner.host_options.run_spec
      base_spec = prior_run_spec if prior_run_spec is not None else AgentRunSpec(
        identity=AgentIdentity(
          agent_id=self._runner.host_options.agent_id or "loop",
          session_id=loop_id,
          is_sub_agent=False,
        ),
        role="custom",
        goal=goal,
      )
      self._runner.host_options.run_spec = _dc_replace(base_spec, loop_round={
        "max_rounds": spec.max_rounds,
        "min_sleep_ms": spec.min_sleep_ms,
        "max_sleep_ms": spec.max_sleep_ms,
        "default_action": spec.default_action,
      })
      # With a RunGroup configured, run() seeds the kernel trap's round base from the group
      # ledger (the driver charges rounds=1 per round below) — max_rounds coercion then happens
      # IN-KERNEL; the check above is the ungrouped backstop.
      try:
        async for evt in self._runner.run(
          session_id=loop_id, goal=goal, criteria=spec.criteria,
        ):
          if spec.on_event is not None:
            spec.on_event(round_no, evt)
          if isinstance(evt, DoneEvent):
            status = evt.status
            pace = evt.pace_decision
      finally:
        self._runner.host_options.run_spec = prior_run_spec

      # Missing pace (old kernel / hard failure): stop and surface — nothing nags.
      decision: dict[str, Any] = pace or {
        "action": "stop",
        "reason": f"round ended without a pace decision (status: {status or 'unknown'})",
      }

      # Cross-round done-gate: a stop proposal may be overridden K times by the judge.
      final_decision = decision
      if (
        final_decision.get("action") == "stop"
        and spec.verdict_fn is not None
        and self._overrides_used < (spec.max_verdict_overrides if spec.max_verdict_overrides is not None else 2)
      ):
        try:
          verdict = spec.verdict_fn({"loop_id": loop_id, "round": round_no, "reason": final_decision.get("reason", "")})
          if inspect.isawaitable(verdict):
            verdict = await verdict
          if not verdict.get("pass"):
            self._overrides_used += 1
            feedback = verdict.get("feedback") or "verdict failed — keep iterating on the goal"
            final_decision = {
              "action": "continue",
              "reason": f"verdict override {self._overrides_used}: {verdict.get('feedback') or 'not done yet'}",
              "coerced_from": f"stop ({final_decision.get('reason', '')})",
            }
        except Exception:
          pass  # judge errs-open: the stop stands

      wake_at_ms = (
        _now_ms() + int(final_decision.get("delay_ms") or 60_000)
        if final_decision.get("action") == "sleep"
        else None
      )
      paced_event: dict[str, Any] = {
        "kind": "round_paced",
        "round": round_no,
        "action": final_decision.get("action"),
        "reason": final_decision.get("reason", ""),
      }
      if final_decision.get("delay_ms") is not None:
        paced_event["delay_ms"] = final_decision["delay_ms"]
      if wake_at_ms is not None:
        paced_event["wake_at_ms"] = wake_at_ms
      if final_decision.get("coerced_from"):
        paced_event["coerced_from"] = final_decision["coerced_from"]
      await log.append(loop_id, paced_event)
      # Lifetime governance: one round = one group charge on the rounds axis.
      group = self._runner.host_options.run_group
      if group is not None:
        await group.budget_store.charge(group.id, rounds=1)

      if final_decision.get("action") == "stop":
        return LoopOutcome(
          loop_id=loop_id, rounds_completed=round_no, stopped=True, state="stopped",
          last_pace=final_decision, last_status=status,
        )
      if final_decision.get("action") == "sleep" and wake_at_ms is not None:
        slept = await self._sleep(wake_at_ms - _now_ms(), wake_at_ms)
        if not slept:
          return LoopOutcome(
            loop_id=loop_id, rounds_completed=round_no, stopped=False, state="dormant",
            last_pace=final_decision, last_status=status, wake_at_ms=wake_at_ms,
          )
      # continue → next round immediately

  async def _sleep(self, delay_ms: int, wake_at_ms: int) -> bool:
    if self._spec.sleeper is not None:
      result = self._spec.sleeper(delay_ms, wake_at_ms)
      if inspect.isawaitable(result):
        result = await result
      return bool(result)
    await asyncio.sleep(max(0, delay_ms) / 1000.0)
    return True


async def run_loop(runner: "RuntimeRunner", spec: LoopSpec) -> LoopOutcome:
  """Facade: run a self-pacing loop agent (joins run_agent/run_fanout as an entry point)."""
  return await LoopDriver(runner, spec).run()
