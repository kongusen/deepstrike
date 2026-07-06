/**
 * L6 — Daily digest: a self-pacing loop agent.
 *
 * A loop agent is NOT a new engine. Each round is exactly one ordinary bounded `RuntimeRunner.run()`
 * (so compaction, RepeatFuse, governance, quota all still apply per round). Three things make it a
 * loop:
 *
 *   • CONTINUITY — every round replays ONE stable `loopId` session, so the transcript carries
 *     forward; round 3 sees what rounds 1–2 wrote.
 *   • PACE — the only genuinely new decision is what happens AFTER a round. The model proposes a
 *     verb (continue / sleep / stop); the kernel's pacing trap adjudicates it (clamping sleep,
 *     coercing to stop at the cap). It surfaces on each round's `done` event as `paceDecision`.
 *     The kernel never sleeps — all timers live here in SDK I/O land.
 *   • DONE-GATE — a cross-round `verdictFn` judges a `stop` proposal; a failing verdict overrides
 *     stop→continue (its feedback steering the next round), up to `maxVerdictOverrides` times.
 *
 * Silence means done: a round that proposes nothing stops (`defaultAction`).
 *
 * New mechanism: Loop agent + pace. Reused: everything from L1 (each round is a normal run).
 *
 * Run:  npx tsx 06-daily-digest/main.ts        (or --dry-run)
 */
import { RuntimeRunner, LocalExecutionPlane, InMemorySessionLog, runLoop, foldLoopState } from "@deepstrike/sdk"
import type { LoopSpec, DoneEvent, TextDelta } from "@deepstrike/sdk"
import { studioTools } from "../shared/studio-tools.js"
import { resolveProvider, parseArgs, loadEnv } from "../shared/provider.js"

async function main(): Promise<void> {
  loadEnv()
  const { flags } = parseArgs(process.argv.slice(2))
  const dryRun = flags["dry-run"] === true

  const plane = new LocalExecutionPlane()
  for (const t of studioTools()) plane.register(t)
  const sessionLog = new InMemorySessionLog()

  const spec: LoopSpec = {
    loopId: "l6-digest", // the ONE session id every round replays
    goal:
      "You are building a running digest of the studio index over several rounds. The sources, in " +
      "order, are: src-cache, src-memory, src-loop, src-workflow, src-signals. " +
      "Each round, do EXACTLY three steps and then END your turn: " +
      "(1) call read_source ONCE on the first id from that list not yet in your digest; " +
      "(2) write one line of text `- <id>: <one-clause summary>`; " +
      "(3) call the `pace` tool with next='continue' (or next='stop' once all five ids are covered). " +
      "Hard rules: call read_source at most ONCE per round. Once a read_source result comes back, your " +
      "VERY NEXT action must be the `pace` tool CALL (a real function call, not text) — do not read " +
      "again, do not repeat a read, do not write the pace as JSON in your message.",
    maxRounds: 4, // hard cap; the kernel coerces continue→stop here no matter what the model wants
    // Cross-round done-gate: refuse an early stop until at least 3 sources are covered. The feedback
    // becomes the next round's steering note. (The per-round criteria gate is the same ladder's lower rung.)
    verdictFn: ({ round, reason }) => {
      const enough = round >= 3
      return enough
        ? { pass: true }
        : { pass: false, feedback: `Only ${round} round(s) done ("${reason.slice(0, 40)}") — cover at least 3 sources before stopping.` }
    },
    maxVerdictOverrides: 2,
    // Injected sleeper: this goal never sleeps, but if a round proposed 'sleep' this is where the
    // wait would happen. Returning true wakes in-process; returning false hands off to an external
    // scheduler and ends run() DORMANT (the cron-loop / dormant-wake path).
    sleeper: async (delayMs) => {
      console.log(`   (sleeper: a round asked to sleep ${delayMs}ms — waking in-process for the demo)`)
      return true
    },
    onEvent: (round, event) => {
      if (event.type === "text_delta") process.stdout.write((event as TextDelta).delta)
      if (event.type === "error") console.log(`\n  ⚠ round ${round} error: ${(event as { error?: string }).error ?? JSON.stringify(event)}`)
      if (event.type === "tool_call") process.stdout.write(`\n  [→ ${(event as { name?: string }).name}]`)
      if (event.type === "done") {
        const p = (event as DoneEvent).paceDecision
        const coerced = p?.coercedFrom ? ` (coerced from ${p.coercedFrom})` : ""
        console.log(`\n  ◀ round ${round} → pace: ${p?.action ?? "—"}${coerced} · ${p?.reason ?? ""}`)
      }
    },
  }

  if (dryRun) {
    console.log("● L6 wiring check (no provider call)")
    console.log(`  loop id     : ${spec.loopId}  (every round replays this one session)`)
    console.log(`  max rounds  : ${spec.maxRounds}  (kernel coerces continue→stop at the cap)`)
    console.log(`  pace verbs  : continue / sleep / stop  (model proposes, kernel adjudicates)`)
    console.log(`  done-gate   : verdictFn refuses stop before round 3 (≤${spec.maxVerdictOverrides} overrides)`)
    console.log("  ✓ set a key and drop --dry-run to watch the loop pace itself round by round.")
    return
  }

  const runner = new RuntimeRunner({
    provider: resolveProvider(),
    executionPlane: plane,
    sessionLog,
    maxTokens: 200_000,
    maxTurns: 6, // per-ROUND turn budget — headroom so a real pace call lands even if the model re-reads
  })

  console.log("━━ self-pacing digest loop ━━ (watch the pace verb after each round)\n")
  const outcome = await runLoop(runner, spec)

  console.log(`\n━━ loop outcome ━━`)
  console.log(`  rounds completed : ${outcome.roundsCompleted}`)
  console.log(`  state            : ${outcome.state}  (last pace: ${outcome.lastPace?.action ?? "—"})`)

  // The pacing record is DURABLE: fold the loop's own session events back into its pacing state
  // (this is what a stateless host would read to resume a dormant loop into its next round).
  const folded = foldLoopState(await sessionLog.read(spec.loopId))
  console.log(`  durable log      : roundsCompleted=${folded.roundsCompleted}, lastPace=${folded.lastPace?.action ?? "—"}`)
  console.log(
    "\nEach round was a normal bounded run over one replayed session; the only new decision was the " +
      "pace verb the kernel adjudicated after each. Silence (or the cap) ends the loop.",
  )
}

main().catch((err) => {
  console.error("\n✗", err instanceof Error ? err.message : err)
  process.exitCode = 1
})
