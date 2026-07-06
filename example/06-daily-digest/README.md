# L6 · Daily digest — a self-pacing loop agent

A loop agent is **not a new engine**. Each round is one ordinary bounded `RuntimeRunner.run()` — the
same compaction, RepeatFuse, governance, and quota you already have. Three things make it a loop.

```
        ┌──────────── one stable loopId session (transcript carries across rounds) ────────────┐
round 1 ─▶ run() ─▶ pace(continue) ─┐   round 2 ─▶ run() ─▶ pace(continue) ─┐   …  ─▶ pace(stop)
                                    └── kernel adjudicates the verb ────────┘
   verdictFn (cross-round done-gate): can override an early stop→continue, feeding back steering
```

## What you learn here

| Mechanism | Where it shows up |
|---|---|
| **Continuity** | Every round replays ONE `loopId` session — round 3 sees the digest rounds 1–2 wrote. The digest visibly grows one line per round. |
| **Pace verb** | The only new decision is what happens AFTER a round: the model calls the `pace` tool (`continue` / `sleep` / `stop`); the kernel's pacing trap adjudicates — clamping a sleep, coercing to stop at `maxRounds`. It surfaces on each round's `done` event as `paceDecision`. |
| **Done-gate** | `verdictFn` judges a `stop` proposal across rounds; a failing verdict overrides stop→continue (up to `maxVerdictOverrides`), its feedback steering the next round. The per-round criteria gate is the same ladder's lower rung. |
| **Durable pacing** | Every round appends `round_started` / `round_paced` to the log; `foldLoopState` reconstructs `roundsCompleted` + last pace — what a stateless host reads to resume a dormant loop. |
| **Dormant / wake** | A `sleep` verb calls the injected `sleeper`. Return `true` to wake in-process; return `false` to hand the wake to an external scheduler and end `run()` **dormant** (the cron-loop path). |

The kernel never sleeps — all timers and judge calls live in SDK I/O land. Silence (a round that
proposes nothing) or the round cap ends the loop.

## A note on the OpenAI-compatible replay fix

The `pace` tool is a **kernel meta-tool**: the kernel consumes the call and answers it with a
synthetic result in its own history, but that result never becomes an execution-plane `tool_completed`
event. On the next round's replay that left the `pace` call unpaired, and strict OpenAI-compatible
providers reject an assistant `tool_call` with no following tool message. The SDK's replay projection
(`pairOrphanToolCalls`) now re-pairs such kernel-consumed orphans — while leaving a genuinely
*pending* tail tool_call (the wake/recovery case) untouched. Without it, a paced loop on an
OpenAI-compatible endpoint dies on round 2; with it, the loop runs clean. (Fixed while building this
level, in **both SDKs** — `pairOrphanToolCalls` in Node and `_pair_orphan_tool_calls` in Python, each
with regression tests; full suites green.)

> Note on the live Python mirror: the loop and the replay fix work identically, but the *pace verb*
> only surfaces when the model reliably emits the `pace` tool call's arguments through the tool
> channel. With a small model that sometimes writes them as message text instead, the round advances
> via the `default_action` / `verdict_fn` ladder and the verb prints as `—` — the same mechanism, one
> rung lower. A capable model shows explicit `pace: continue`/`stop` as the Node run does.

## Run

```sh
npx tsx 06-daily-digest/main.ts            # a 4-round digest loop that paces itself
npx tsx 06-daily-digest/main.ts --dry-run  # wiring only
../../python/.venv/bin/python 06-daily-digest/main.py   # the Python mirror
```

You'll see `pace: continue` after rounds 1–3 (each adding a digest line) and a clean `pace: stop`
once every source is covered — the full verb vocabulary, adjudicated by the kernel.

## What's next

**L7 · Workflow DAG** stops being one agent: a declarative DAG fans out sub-agents (spawn / loop /
classify / tournament / a host-computed reduce), each node's output typed by a schema, with a Harness
gate and Milestones marking progress — orchestration as data.
