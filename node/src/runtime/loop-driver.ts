/**
 * ③ Dynamic loop-agent engineering system — the SDK driver.
 *
 * A loop agent is NOT a new execution engine:
 * - a ROUND is exactly one bounded `RuntimeRunner.run()` (compaction, RepeatFuse,
 *   criteria gate, and budget verdicts all apply per round for free);
 * - CONTINUITY is the session log replayed under ONE stable sessionId;
 * - LIFETIME GOVERNANCE is the RunGroup the rounds are members of;
 * - the only new decision — what happens AFTER a round — is the model-proposed,
 *   kernel-adjudicated `pace` verb (see the kernel pacing trap). The kernel never
 *   sleeps; all timers and judge calls live here, in SDK I/O land.
 *
 * Durable pacing: every round appends `round_started` / `round_paced` to the loop's
 * session log, so `LoopDriver.resume()`-style recovery is a fold over the log
 * — zero new storage. A stateless host
 * reads `wake_at_ms` from the fold and re-arms via its own cron/queue; an
 * in-process host lets `run()` sleep inline.
 */
import type { RuntimeRunner } from "./runner.js"
import type { SessionEvent } from "./session-log.js"
import type { PaceDecision } from "./kernel-step.js"
import type { DoneEvent, StreamEvent } from "../types.js"

export interface LoopSpec {
  /** Stable loop id = the ONE session id every round replays (transcript continuity). */
  loopId: string
  goal: string
  criteria?: string[]
  /** Hard round cap; the kernel coerces continue/sleep to stop at the cap. */
  maxRounds?: number
  /** Sleep clamp bounds (ms), enforced in-kernel. */
  minSleepMs?: number
  maxSleepMs?: number
  /** "stop" (goal loop, default) | "sleep" (cron loop) when a round never calls pace. */
  defaultAction?: "stop" | "sleep"
  /** Cross-round done-gate: judges a stop proposal; a failing verdict overrides
   *  stop→continue at most `maxVerdictOverrides` times, its feedback becoming the
   *  next round's steering note. The in-kernel O4 criteria gate is the per-round rung
   *  of the same ladder — this is the cross-round rung. */
  verdictFn?: (ctx: { loopId: string; round: number; reason: string }) =>
    | Promise<{ pass: boolean; feedback?: string }>
    | { pass: boolean; feedback?: string }
  maxVerdictOverrides?: number
  /** Sleep implementation (injectable for tests / stateless hosts). Default: setTimeout.
   *  Return `false` to hand the wake to an external scheduler and end `run()` dormant. */
  sleeper?: (delayMs: number, wakeAtMs: number) => Promise<boolean>
  /** Per-round event tap (streaming passthrough). */
  onEvent?: (round: number, event: StreamEvent) => void
}

export interface LoopOutcome {
  loopId: string
  roundsCompleted: number
  stopped: boolean
  /** "stopped" | "dormant" (sleeper handed off to an external scheduler) */
  state: "stopped" | "dormant"
  lastPace?: PaceDecision
  lastStatus?: string
  /** Absolute wake time when dormant. */
  wakeAtMs?: number
}

/** Fold the loop's session log into resumable pacing state — zero new storage. DW-5: the judge's
 *  override budget folds too, so a crash/restart can't grant the verdictFn fresh overrides. */
export function foldLoopState(events: Array<{ seq: number; event: SessionEvent }>): {
  roundsCompleted: number
  pendingWakeAtMs?: number
  lastPace?: { action: string; reason: string }
  overridesUsed: number
} {
  let roundsCompleted = 0
  let pendingWakeAtMs: number | undefined
  let lastPace: { action: string; reason: string } | undefined
  let overridesUsed = 0
  for (const { event } of events) {
    if (event.kind === "round_paced") {
      roundsCompleted = Math.max(roundsCompleted, event.round)
      lastPace = { action: event.action, reason: event.reason }
      pendingWakeAtMs = event.action === "sleep" ? event.wake_at_ms : undefined
      if (event.reason.startsWith("verdict override")) overridesUsed += 1
    }
  }
  return { roundsCompleted, pendingWakeAtMs, lastPace, overridesUsed }
}

/**
 * DW-6 completion→wake bridge, composed from two existing seams (zero new mechanism): a `sleeper`
 * that races the timer against an L0 recipient-addressed signal on the shared gateway. Ingest a
 * signal with `recipient: loopId` (a subagent/workflow completion, a webhook) and the sleeping loop
 * wakes into its next round immediately — where the SAME queued signal then reaches the model
 * through the kernel's normal signal path, so the wake reason is visible in-round.
 */
export function signalAwareSleeper(
  gateway: { onSignal(listener: (sig: { recipient?: string }) => void): () => void },
  loopId: string,
): NonNullable<LoopSpec["sleeper"]> {
  return (delayMs: number) =>
    new Promise<boolean>(resolve => {
      let settled = false
      const settle = (v: boolean) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        unsubscribe()
        resolve(v)
      }
      const unsubscribe = gateway.onSignal(sig => {
        if (sig.recipient === loopId) settle(true)
      })
      const timer = setTimeout(() => settle(true), Math.max(0, delayMs))
    })
}

export class LoopDriver {
  private overridesUsed = 0

  constructor(
    private readonly runner: RuntimeRunner,
    private readonly spec: LoopSpec,
  ) {}

  /**
   * Drive rounds until the loop stops or goes dormant. Resumable by construction:
   * the round count and any pending wake are folded from the session log, so
   * calling `run()` again after a crash / on a stateless host continues in place.
   */
  async run(): Promise<LoopOutcome> {
    const { loopId } = this.spec
    const log = this.runner.hostOptions.sessionLog

    // Resume: fold prior rounds + pending wake + the judge's used overrides from the transcript
    // (DW-5: a crash/restart must not refill the verdictFn's override budget).
    const prior = foldLoopState(await log.read(loopId))
    let round = prior.roundsCompleted
    this.overridesUsed = Math.max(this.overridesUsed, prior.overridesUsed)
    if (prior.pendingWakeAtMs !== undefined) {
      const remaining = prior.pendingWakeAtMs - Date.now()
      if (remaining > 0) {
        const slept = await this.sleep(remaining, prior.pendingWakeAtMs)
        if (!slept) {
          return {
            loopId, roundsCompleted: round, stopped: false,
            state: "dormant", wakeAtMs: prior.pendingWakeAtMs,
          }
        }
      }
    }

    let feedback: string | undefined
    for (;;) {
      round += 1
      // Driver-side round-cap backstop: with a RunGroup, the kernel trap coerces via the
      // seeded ledger; without one, this is the only max_rounds enforcement point.
      if (this.spec.maxRounds !== undefined && round > this.spec.maxRounds) {
        return {
          loopId, roundsCompleted: round - 1, stopped: true, state: "stopped",
          lastPace: { action: "stop", reason: `max_rounds=${this.spec.maxRounds} exhausted` },
        }
      }
      await log.append(loopId, { kind: "round_started", round, goal: this.spec.goal })

      const goal = feedback
        ? `${this.spec.goal}\n\n[LOOP FEEDBACK round ${round - 1}] ${feedback}`
        : this.spec.goal
      feedback = undefined

      let pace: PaceDecision | undefined
      let status: string | undefined
      // ONE round = one bounded kernel run under the stable loop session id. The
      // kernel's pacing trap adjudicates the model's pace proposal; we consume it
      // from the done event. runSpec.loopRound arms the trap + the pace tool.
      const priorRunSpec = this.runner.hostOptions.runSpec
      this.runner.hostOptions.runSpec = {
        identity: { agentId: this.runner.hostOptions.agentId ?? "loop", sessionId: loopId, isSubAgent: false },
        role: "custom",
        goal,
        ...(priorRunSpec ?? {}),
        loopRound: {
          maxRounds: this.spec.maxRounds,
          minSleepMs: this.spec.minSleepMs,
          maxSleepMs: this.spec.maxSleepMs,
          defaultAction: this.spec.defaultAction,
        },
      }
      // With a RunGroup configured, run() reserves one round and the kernel reports its
      // correlated local usage; the check above remains the ungrouped backstop.
      try {
        for await (const evt of this.runner.run({
          sessionId: loopId,
          goal,
          criteria: this.spec.criteria,
        })) {
          this.spec.onEvent?.(round, evt)
          if (evt.type === "done") {
            const d = evt as DoneEvent
            status = d.status
            pace = d.paceDecision
          }
        }
      } finally {
        this.runner.hostOptions.runSpec = priorRunSpec
      }

      // Missing pace (old kernel / hard failure): stop and surface — nothing nags.
      const decision: PaceDecision = pace ?? {
        action: "stop",
        reason: `round ended without a pace decision (status: ${status ?? "unknown"})`,
      }

      // Cross-round done-gate: a stop proposal may be overridden K times by the judge.
      let finalDecision = decision
      if (
        finalDecision.action === "stop"
        && this.spec.verdictFn
        && this.overridesUsed < (this.spec.maxVerdictOverrides ?? 2)
      ) {
        try {
          const verdict = await this.spec.verdictFn({ loopId, round, reason: finalDecision.reason })
          if (!verdict.pass) {
            this.overridesUsed += 1
            feedback = verdict.feedback ?? "verdict failed — keep iterating on the goal"
            finalDecision = {
              action: "continue",
              reason: `verdict override ${this.overridesUsed}: ${verdict.feedback ?? "not done yet"}`,
              coercedFrom: `stop (${finalDecision.reason})`,
            }
          }
        } catch { /* judge errs-open: the stop stands */ }
      }

      const wakeAtMs = finalDecision.action === "sleep"
        ? Date.now() + (finalDecision.delayMs ?? 60_000)
        : undefined
      await log.append(loopId, {
        kind: "round_paced",
        round,
        action: finalDecision.action,
        ...(finalDecision.delayMs !== undefined ? { delay_ms: finalDecision.delayMs } : {}),
        ...(wakeAtMs !== undefined ? { wake_at_ms: wakeAtMs } : {}),
        reason: finalDecision.reason,
        ...(finalDecision.coercedFrom ? { coerced_from: finalDecision.coercedFrom } : {}),
      })
      if (finalDecision.action === "stop") {
        return {
          loopId, roundsCompleted: round, stopped: true, state: "stopped",
          lastPace: finalDecision, lastStatus: status,
        }
      }
      if (finalDecision.action === "sleep" && wakeAtMs !== undefined) {
        const slept = await this.sleep(wakeAtMs - Date.now(), wakeAtMs)
        if (!slept) {
          return {
            loopId, roundsCompleted: round, stopped: false, state: "dormant",
            lastPace: finalDecision, lastStatus: status, wakeAtMs,
          }
        }
      }
      // continue → next round immediately
    }
  }

  private sleep(delayMs: number, wakeAtMs: number): Promise<boolean> {
    if (this.spec.sleeper) return Promise.resolve(this.spec.sleeper(delayMs, wakeAtMs))
    return new Promise(resolve => setTimeout(() => resolve(true), Math.max(0, delayMs)))
  }
}

/** Facade: run a self-pacing loop agent (joins runAgent/runFanout as an entry point). */
export async function runLoop(runner: RuntimeRunner, spec: LoopSpec): Promise<LoopOutcome> {
  return new LoopDriver(runner, spec).run()
}
