/**
 * Canonical-ABI Task 0 — pre-migration stop-the-bleeding fixes for the effect lifecycle.
 *
 * R-B27: `evaluate_milestone` with neither a phase `verifier` nor an `onMilestoneEvaluate` hook
 *        used to `return` straight out of the loop, leaving the milestone effect alive in the
 *        kernel's pending table forever. It must now feed back a conservative resolution
 *        (`passed: false`) — pending cleared, phase NOT advanced.
 * R-B28: the main loop's `if / else if` chain had no `else`. An effect kind with no branch at that
 *        position left `action` unreplaced and no event in flight, so `while (!isTerminal())` spun
 *        at 100% CPU forever. It must now terminate the run with an explicit error.
 */
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../src/runtime/execution-plane.js"
import { MILESTONE_UNVERIFIED_REASON } from "../src/types/agent.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const provider: LLMProvider = {
  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  },
  async *stream(_context: RenderedContext): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "done" }
  },
}

function makeRunner(sessionLog: InMemorySessionLog, extra: Record<string, unknown> = {}) {
  return new RuntimeRunner({
    provider,
    sessionLog,
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 4000,
    maxTurns: 8,
    ...extra,
  } as never)
}

describe("R-B27: an unverifiable milestone effect is still resolved", () => {
  it("feeds back a fail-closed milestone result instead of leaking the pending effect", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = makeRunner(sessionLog, {
      milestoneContract: { phases: [{ id: "phase1", criteria: ["must complete"] }] },
      milestonePolicy: "require_verifier",
    })

    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "milestone-leak", goal: "test" })) {
      events.push(evt)
    }

    const done = events.filter(e => e.type === "done")
    expect(done).toHaveLength(1)
    expect((done[0] as { status: string }).status).toBe("milestone_pending")

    const logged = await sessionLog.read("milestone-leak")
    // The kernel accepted the resolution — it only emits `milestone_blocked` from
    // `handle_milestone_result`, which is also where the pending effect is removed.
    const blocked = logged
      .map(e => e.event)
      .find(e => e.kind === "milestone_blocked") as { phase_id: string; reason: string } | undefined
    expect(blocked).toBeDefined()
    expect(blocked!.phase_id).toBe("phase1")
    expect(blocked!.reason).toBe(MILESTONE_UNVERIFIED_REASON)

    // Fail-closed: the phase did NOT advance and no capability was unlocked.
    expect(logged.some(e => e.event.kind === "milestone_advanced")).toBe(false)
  })
})

describe("R-B28: the main loop cannot busy-wait on an effect it has no branch for", () => {
  // Jest's per-test timeout is the busy-wait detector: before the `else` backstop this run never
  // returned (the loop re-entered on an unreplaced `action` with no event in flight).
  it("terminates the run with an explicit error", async () => {
    const sessionLog = new InMemorySessionLog()
    const runner = makeRunner(sessionLog)

    // Forge the situation the audit describes: an effect that IS part of the action union but is
    // only ever driven inside the workflow driver arrives at the main-loop position. Swapping the
    // mapped action (not the kernel's own step) keeps the kernel/session-log transaction chain
    // intact while reproducing exactly what the loop sees.
    const priv = runner as unknown as {
      commitKernelAction: (...args: unknown[]) => Promise<{ kind: string; effectId: string }>
    }
    const original = priv.commitKernelAction.bind(runner)
    let forged = false
    priv.commitKernelAction = async (...args: unknown[]) => {
      const action = await original(...args)
      if (!forged && action.kind === "call_provider") {
        forged = true
        return {
          kind: "preempt_sub_agents",
          effectId: action.effectId,
          agentIds: ["ghost-agent"],
          reason: "test",
        }
      }
      return action
    }

    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "unhandled-effect", goal: "test" })) {
      events.push(evt)
    }

    expect(forged).toBe(true)
    const errors = events.filter(e => e.type === "error") as Array<{ message: string }>
    expect(errors).toHaveLength(1)
    expect(errors[0].message).toContain("unhandled kernel effect preempt_sub_agents")

    const done = events.filter(e => e.type === "done")
    expect(done).toHaveLength(1)
    expect((done[0] as { status: string }).status).toBe("error")

    const logged = await sessionLog.read("unhandled-effect")
    const terminal = logged
      .map(e => e.event)
      .find(e => e.kind === "run_terminal") as { reason: string } | undefined
    expect(terminal?.reason).toBe("error")
  }, 15_000)
})
