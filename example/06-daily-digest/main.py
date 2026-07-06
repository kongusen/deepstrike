"""L6 — Daily digest: a self-pacing loop agent (Python mirror of 06-daily-digest/main.ts).

A loop agent is NOT a new engine. Each round is exactly one ordinary bounded RuntimeRunner.run().
Three things make it a loop:
  • CONTINUITY — every round replays ONE stable loop_id session, so the transcript carries forward.
  • PACE — the only new decision is what happens AFTER a round: the model calls the `pace` tool
    (continue / sleep / stop); the kernel's pacing trap adjudicates it (clamp sleep, coerce to stop at
    the cap). It surfaces on each round's `done` event as `pace_decision`.
  • DONE-GATE — a cross-round `verdict_fn` judges a `stop` proposal; a failing verdict overrides
    stop→continue (its feedback steering the next round), up to `max_verdict_overrides` times.

Silence means done: a round that proposes nothing stops (`default_action`).

Run (from this directory):
    ../../python/.venv/bin/python main.py
    ../../python/.venv/bin/python main.py --dry-run
"""
from __future__ import annotations

import asyncio
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
EXAMPLE_ROOT = Path(__file__).resolve().parent.parent


def load_env() -> None:
    for p in (EXAMPLE_ROOT / ".env", EXAMPLE_ROOT.parent / ".env"):
        if p.exists():
            for line in p.read_text(encoding="utf-8").splitlines():
                line = line.strip()
                if not line or line.startswith("#") or "=" not in line:
                    continue
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip())
            return


from deepstrike import (  # noqa: E402
    RuntimeRunner, RuntimeOptions, LocalExecutionPlane, InMemorySessionLog,
    AnthropicProvider, OpenAIProvider,
    LoopSpec, run_loop, fold_loop_state,
    TextDelta, DoneEvent, ErrorEvent, ToolCallEvent,
)
from shared.studio_tools import studio_tools  # noqa: E402


def resolve_provider():
    if os.environ.get("ANTHROPIC_API_KEY"):
        model = os.environ.get("DEEPSTRIKE_MODEL")
        return AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"], **({"model": model} if model else {}))
    if os.environ.get("OPENAI_API_KEY"):
        model = os.environ.get("DEEPSTRIKE_MODEL") or os.environ.get("OPENAI_MODEL")
        base_url = os.environ.get("DEEPSTRIKE_BASE_URL") or os.environ.get("OPENAI_BASE_URL")
        kw = {}
        if model:
            kw["model"] = model
        if base_url:
            kw["base_url"] = base_url
        return OpenAIProvider(api_key=os.environ["OPENAI_API_KEY"], **kw)
    raise SystemExit("No provider configured. Set OPENAI_API_KEY (or ANTHROPIC_API_KEY), or pass --dry-run.")


async def verdict_fn(ctx: dict) -> dict:
    """Cross-round done-gate: refuse an early stop until at least 3 sources are covered. The feedback
    becomes the next round's steering note (the per-round criteria gate is the same ladder's lower rung)."""
    rnd = ctx.get("round", 0)
    if rnd >= 3:
        return {"pass": True}
    reason = str(ctx.get("reason", ""))[:40]
    return {"pass": False, "feedback": f'Only {rnd} round(s) done ("{reason}") — cover at least 3 sources before stopping.'}


async def sleeper(delay_ms: int, wake_at_ms: int) -> bool:
    """This goal never sleeps, but if a round proposed 'sleep' this is where the wait would happen.
    Return True to wake in-process; return False to hand off to an external scheduler (dormant)."""
    print(f"   (sleeper: a round asked to sleep {delay_ms}ms — waking in-process for the demo)")
    return True


def on_event(rnd: int, event) -> None:
    if isinstance(event, TextDelta):
        sys.stdout.write(event.delta)
        sys.stdout.flush()
    elif isinstance(event, ErrorEvent):
        print(f"\n  ⚠ round {rnd} error: {getattr(event, 'error', None) or getattr(event, 'message', event)}")
    elif isinstance(event, ToolCallEvent):
        sys.stdout.write(f"\n  [→ {event.name}]")
    elif isinstance(event, DoneEvent):
        p = event.pace_decision
        action = getattr(p, "action", None) if p else None
        reason = getattr(p, "reason", "") if p else ""
        coerced = getattr(p, "coerced_from", None) if p else None
        tail = f" (coerced from {coerced})" if coerced else ""
        print(f"\n  ◀ round {rnd} → pace: {action or '—'}{tail} · {reason}")


async def main() -> None:
    load_env()
    dry_run = "--dry-run" in sys.argv[1:]

    plane = LocalExecutionPlane()
    for t in studio_tools():
        plane.register(t)
    session_log = InMemorySessionLog()

    spec = LoopSpec(
        loop_id="l6-digest",  # the ONE session id every round replays
        goal=(
            "You are building a running digest of the studio index over several rounds. The sources, in "
            "order, are: src-cache, src-memory, src-loop, src-workflow, src-signals. "
            "Each round, do EXACTLY three steps and then END your turn: "
            "(1) call read_source ONCE on the first id from that list not yet in your digest; "
            "(2) write one line of text `- <id>: <one-clause summary>`; "
            "(3) call the `pace` tool with next='continue' (or next='stop' once all five ids are covered). "
            "Hard rules: call read_source at most ONCE per round. Once a read_source result comes back, your "
            "VERY NEXT action must be the `pace` tool CALL (a real function call, not text) — do not read "
            "again, do not repeat a read, do not write the pace as JSON in your message."
        ),
        max_rounds=4,  # hard cap; the kernel coerces continue→stop at the cap
        verdict_fn=verdict_fn,
        max_verdict_overrides=2,
        sleeper=sleeper,
        on_event=on_event,
    )

    if dry_run:
        print("● L6 wiring check (no provider call)")
        print(f"  loop id     : {spec.loop_id}  (every round replays this one session)")
        print(f"  max rounds  : {spec.max_rounds}  (kernel coerces continue→stop at the cap)")
        print("  pace verbs  : continue / sleep / stop  (model proposes, kernel adjudicates)")
        print(f"  done-gate   : verdict_fn refuses stop before round 3 (≤{spec.max_verdict_overrides} overrides)")
        print("  ✓ set a key and drop --dry-run to watch the loop pace itself round by round.")
        return

    runner = RuntimeRunner(RuntimeOptions(
        provider=resolve_provider(),
        execution_plane=plane,
        session_log=session_log,
        max_tokens=200_000,
        max_turns=6,  # per-ROUND turn budget (each round is a normal bounded run)
    ))

    print("━━ self-pacing digest loop ━━ (watch the pace verb after each round)\n")
    outcome = await run_loop(runner, spec)

    print("\n━━ loop outcome ━━")
    print(f"  rounds completed : {outcome.rounds_completed}")
    last = getattr(outcome, "last_pace", None)
    print(f"  state            : {outcome.state}  (last pace: {getattr(last, 'action', None) or '—'})")

    # The pacing record is DURABLE: fold the loop's own session events back into its pacing state.
    folded = fold_loop_state(await session_log.read(spec.loop_id))
    fp = getattr(folded, "last_pace", None)
    print(f"  durable log      : rounds_completed={folded.rounds_completed}, last_pace={getattr(fp, 'action', None) or '—'}")
    print("\nEach round was a normal bounded run over one replayed session; the only new decision was the "
          "pace verb the kernel adjudicated after each. Silence (or the cap) ends the loop.")


if __name__ == "__main__":
    asyncio.run(main())
