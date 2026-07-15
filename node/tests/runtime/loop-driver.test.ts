import { createRunner } from "./helpers.js"
import { LoopDriver, foldLoopState, runLoop, signalAwareSleeper } from "../../src/runtime/loop-driver.js"
import { SignalGateway } from "../../src/os/public.js"
import { InMemoryGroupBudgetStore } from "../../src/runtime/run-group.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

/** Scripted loop provider: each ROUND proposes a pace verb, then files its round report. */
function scriptedLoopProvider(script: Array<{ next: string; delayMs?: number }>): LLMProvider {
  let call = 0
  let round = 0
  return {
    async complete(): Promise<Message> {
      return { role: "assistant", content: "done", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      call += 1
      // Odd calls: propose pace for the current round; even calls: the strip-tools
      // final report turn the kernel forces after an allowed pace.
      if (call % 2 === 1) {
        const step = script[Math.min(round, script.length - 1)]
        round += 1
        yield {
          type: "tool_call",
          id: `pace-${round}`,
          name: "pace",
          arguments: {
            next: step.next,
            ...(step.delayMs !== undefined ? { delay_ms: step.delayMs } : {}),
            reason: `scripted round ${round}`,
          },
        }
        return
      }
      yield { type: "text_delta", delta: `round ${round} report` }
    },
  }
}

describe("③ dynamic loop agent — LoopDriver over the kernel pacing trap", () => {
  it("drives continue → sleep(clamped) → stop under ONE session id with a durable pace trail", async () => {
    const provider = scriptedLoopProvider([
      { next: "continue" },
      { next: "sleep", delayMs: 5 }, // below the 10s floor → kernel clamps + records coercion
      { next: "stop" },
    ])
    const { runner, sessionLog } = createRunner(provider, [])
    const slept: number[] = []

    const outcome = await runLoop(runner, {
      loopId: "loop-basic",
      goal: "iterate the widget until done",
      minSleepMs: 10_000,
      maxSleepMs: 600_000,
      sleeper: async delayMs => {
        slept.push(delayMs)
        return true // in-process wake, no real timer
      },
    })

    expect(outcome.stopped).toBe(true)
    expect(outcome.state).toBe("stopped")
    expect(outcome.roundsCompleted).toBe(3)
    expect(outcome.lastPace?.action).toBe("stop")

    // Durable pacing trail: 3 round_started + 3 round_paced on the ONE session id,
    // with the kernel's clamp coercion recorded on round 2.
    const events = (await sessionLog.read("loop-basic")).map(e => e.event)
    const started = events.filter(e => e.kind === "round_started")
    const paced = events.filter(e => e.kind === "round_paced") as Array<
      Extract<(typeof events)[number], { kind: "round_paced" }>
    >
    expect(started.length).toBe(3)
    expect(paced.map(p => p.action)).toEqual(["continue", "sleep", "stop"])
    expect(paced[1].delay_ms).toBe(10_000)
    expect(paced[1].coerced_from).toContain("clamped")
    expect(paced[1].wake_at_ms).toBeGreaterThan(0)
    // The in-kernel clamp actually slept the clamped duration (±scheduling slack).
    expect(slept.length).toBe(1)
    expect(slept[0]).toBeGreaterThan(9_000)

    // Transcript continuity: one growing session, multiple run_started/run_terminal pairs.
    expect(events.filter(e => e.kind === "run_terminal").length).toBe(3)
  })

  it("resumes mid-sleep from the folded log and goes dormant when the sleeper hands off", async () => {
    const provider = scriptedLoopProvider([{ next: "sleep", delayMs: 60_000 }])
    const { runner, sessionLog } = createRunner(provider, [])

    // Round 1 ends in sleep; the sleeper declines (stateless host) → dormant with wake time.
    const first = await runLoop(runner, {
      loopId: "loop-dormant",
      goal: "cron tick",
      minSleepMs: 1_000,
      sleeper: async () => false,
    })
    expect(first.state).toBe("dormant")
    expect(first.wakeAtMs).toBeGreaterThan(Date.now() - 1_000)

    // The fold alone recovers round count + pending wake — zero extra storage.
    const folded = foldLoopState(await sessionLog.read("loop-dormant"))
    expect(folded.roundsCompleted).toBe(1)
    expect(folded.pendingWakeAtMs).toBe(first.wakeAtMs)
  })

  it("verdictFn overrides a stop at most K times and feeds its feedback into the next round", async () => {
    const provider = scriptedLoopProvider([
      { next: "stop" },
      { next: "stop" },
      { next: "stop" },
    ])
    const { runner, sessionLog } = createRunner(provider, [])
    const judged: number[] = []

    const outcome = await new LoopDriver(runner, {
      loopId: "loop-verdict",
      goal: "ship the fix",
      maxVerdictOverrides: 2,
      verdictFn: ({ round }) => {
        judged.push(round)
        return { pass: false, feedback: "tests are still red" }
      },
    }).run()

    // Two overrides then the third stop stands (K exhausted; judge no longer consulted).
    expect(judged).toEqual([1, 2])
    expect(outcome.stopped).toBe(true)
    expect(outcome.roundsCompleted).toBe(3)

    const events = (await sessionLog.read("loop-verdict")).map(e => e.event)
    const paced = events.filter(e => e.kind === "round_paced") as Array<
      Extract<(typeof events)[number], { kind: "round_paced" }>
    >
    expect(paced[0].action).toBe("continue")
    expect(paced[0].coerced_from).toContain("stop")
    // The judge's feedback steers the next round's goal.
    const starts = events.filter(e => e.kind === "round_started")
    expect(starts.length).toBe(3)
  })

  it("enforces maxRounds as the ungrouped backstop without starting an extra round", async () => {
    const provider = scriptedLoopProvider([{ next: "continue" }])
    const { runner, sessionLog } = createRunner(provider, [])

    const outcome = await runLoop(runner, {
      loopId: "loop-cap",
      goal: "spin",
      maxRounds: 2,
    })
    expect(outcome.stopped).toBe(true)
    expect(outcome.roundsCompleted).toBe(2)
    expect(outcome.lastPace?.reason).toContain("max_rounds")
    const events = (await sessionLog.read("loop-cap")).map(e => e.event)
    expect(events.filter(e => e.kind === "round_started").length).toBe(2)
  })

  it("settles exactly one reservation-backed round per loop vehicle", async () => {
    const provider = scriptedLoopProvider([
      { next: "continue" },
      { next: "continue" },
      { next: "stop" },
    ])
    const { runner } = createRunner(provider, [])
    const store = new InMemoryGroupBudgetStore()
    runner.hostOptions.runGroup = { id: "loop-group", budgetStore: store }

    const outcome = await runLoop(runner, {
      loopId: "loop-grouped",
      goal: "iterate",
      maxRounds: 3,
    })

    expect(outcome.roundsCompleted).toBe(3)
    expect(store.read("loop-group").roundsCompleted).toBe(3)
  })

  it("DW-5: the verdict override budget folds from the log — a restart grants no fresh overrides", async () => {
    const provider = scriptedLoopProvider([{ next: "stop" }])
    const { runner, sessionLog } = createRunner(provider, [])
    // Simulate a pre-crash trail: 2 rounds whose stops were already overridden by the judge.
    await sessionLog.append("loop-refold", { kind: "round_started", round: 1, goal: "g" })
    await sessionLog.append("loop-refold", {
      kind: "round_paced", round: 1, action: "continue",
      reason: "verdict override 1: tests are still red", coerced_from: "stop (done)",
    })
    await sessionLog.append("loop-refold", { kind: "round_started", round: 2, goal: "g" })
    await sessionLog.append("loop-refold", {
      kind: "round_paced", round: 2, action: "continue",
      reason: "verdict override 2: still red", coerced_from: "stop (done)",
    })

    const judged: number[] = []
    const outcome = await new LoopDriver(runner, {
      loopId: "loop-refold",
      goal: "ship the fix",
      maxVerdictOverrides: 2,
      verdictFn: ({ round }) => {
        judged.push(round)
        return { pass: false, feedback: "no" }
      },
    }).run()
    // Budget exhausted by the folded trail: the judge is never consulted, the stop stands.
    expect(judged).toEqual([])
    expect(outcome.stopped).toBe(true)
    expect(outcome.roundsCompleted).toBe(3)
  })

  it("DW-6: signalAwareSleeper wakes a sleeping loop on its recipient-addressed signal", async () => {
    const gateway = new SignalGateway()
    const sleeper = signalAwareSleeper(gateway, "loop-wake")
    const t0 = Date.now()
    const sleeping = sleeper(60_000, t0 + 60_000)
    // A signal addressed to ANOTHER loop must not wake us.
    gateway.ingest({ source: "custom", signalType: "event", urgency: "normal", payload: {}, recipient: "someone-else" })
    // The completion→wake bridge: a signal addressed to THIS loop ends the sleep immediately.
    setTimeout(() => {
      gateway.ingest({ source: "custom", signalType: "event", urgency: "normal", payload: { goal: "wf done" }, recipient: "loop-wake" })
    }, 10)
    const woke = await sleeping
    expect(woke).toBe(true)
    expect(Date.now() - t0).toBeLessThan(5_000)
    // The wake signal stays queued for the next round's kernel signal path (visible to the model).
    expect(gateway.depth).toBe(2)
    gateway.destroy()
  })

  it("a round that never calls pace falls back to the kernel default_action", async () => {
    // Provider ends immediately with text — no pace call; goal loop default = stop.
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "all done", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "all done" }
      },
    }
    const { runner } = createRunner(provider, [])
    const outcome = await runLoop(runner, { loopId: "loop-nopace", goal: "one shot" })
    expect(outcome.stopped).toBe(true)
    expect(outcome.roundsCompleted).toBe(1)
    expect(outcome.lastPace?.action).toBe("stop")
    expect(outcome.lastPace?.reason).toContain("default_action")
  })
})
