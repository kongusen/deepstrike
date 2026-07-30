/**
 * Canonical-ABI Task 0 — pre-migration stop-the-bleeding fixes for the effect lifecycle (wasm).
 *
 * R-B27: `evaluate_milestone` with neither a phase `verifier` nor an `onMilestoneEvaluate` hook
 *        used to `return` straight out of the loop, leaving the milestone effect alive in the
 *        kernel's pending table forever. It must now feed back a conservative resolution
 *        (`passed: false`) — pending cleared, phase NOT advanced.
 * R-B28: the main loop's `if / else if` chain had no `else`, so an effect kind with no branch at
 *        that position left `action` unreplaced and no event in flight ⇒ 100% CPU busy-wait.
 * R-B32: `spool_large_result` built a `new LargeResultSpool()` per effect (default in-memory
 *        driver), so an unconfigured host handed the kernel a well-formed `spool_ref` whose
 *        content was garbage-collected before anyone could read it back.
 *
 * WASM has no native kernel under test — the shared mock (`tests/__mocks__/kernel.ts`) stands in.
 * Each case scripts the exact effect sequence by wrapping the mock's `step`, which keeps the
 * scenarios readable and leaves the shared mock untouched for every other suite.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import { MILESTONE_UNVERIFIED_REASON } from "../src/runtime/types/agent.js"
import type { LLMProvider, StreamEvent } from "../src/types.js"
import { KernelRuntime } from "@deepstrike/wasm-kernel"

type Step = { version: number; actions: Array<Record<string, unknown>>; observations: Array<Record<string, unknown>>; faults: unknown[] }
/** Return a step to script this event, or `undefined` to let the shared mock kernel answer. */
type Scripted = (event: Record<string, unknown>) => Step | undefined

const provider: LLMProvider = {
  async complete() {
    return { role: "assistant", content: "done", toolCalls: [] }
  },
  async *stream() {
    yield { type: "text_delta", delta: "done" }
  },
}

/** Wrap the mock kernel's `step` for the duration of one test. The hook is consulted *before* the
 *  mock runs — the mock's own transitions have side effects (it flips itself terminal on a
 *  tool-less `provider_result`), so a scripted event must short-circuit it, not post-process it. */
function scriptKernel(script: Scripted): () => void {
  const proto = KernelRuntime.prototype as unknown as { step: (input: string) => string }
  const original = proto.step
  proto.step = function patched(this: unknown, input: string): string {
    const { event } = JSON.parse(input) as { event: Record<string, unknown> }
    const scripted = script(event)
    return scripted ? JSON.stringify(scripted) : original.call(this, input)
  }
  return () => { proto.step = original }
}

const reply = (actions: Array<Record<string, unknown>>, observations: Array<Record<string, unknown>> = []): Step =>
  ({ version: 2, actions, observations, faults: [] })

function makeRunner(sessionLog: InMemorySessionLog, extra: Record<string, unknown> = {}) {
  return new RuntimeRunner({
    provider,
    sessionLog,
    executionPlane: new LocalExecutionPlane(),
    maxTokens: 4000,
    maxTurns: 6,
    ...extra,
  } as never)
}

describe("R-B27: an unverifiable milestone effect is still resolved", () => {
  let restore = () => {}
  afterEach(() => restore())

  it("feeds back a fail-closed milestone result instead of leaking the pending effect", async () => {
    const milestoneResults: Array<Record<string, unknown>> = []
    restore = scriptKernel(event => {
      if (event.kind === "provider_result") {
        // Stand in for the kernel deciding the current phase needs attestation.
        return reply([{ kind: "evaluate_milestone", effect_id: "ms-1", phase_id: "phase1", criteria: ["ship it"], required_evidence: [] }])
      }
      if (event.kind === "milestone_result") {
        milestoneResults.push(event.result as Record<string, unknown>)
        // Mirrors the real kernel: a failed check blocks the phase and re-enters reasoning.
        return reply(
          [{ kind: "call_provider", effect_id: "p-2", context: { systemText: "", turns: [] }, tools: [] }],
          [{ kind: "milestone_blocked", turn: 1, phase_id: "phase1", reason: String((event.result as { reason?: string }).reason ?? "") }],
        )
      }
      return undefined
    })

    const sessionLog = new InMemorySessionLog()
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(sessionLog).run({ sessionId: "ms-leak", goal: "test" })) {
      events.push(evt)
    }

    // The effect was resolved — exactly one conservative result, phase not advanced.
    expect(milestoneResults).toHaveLength(1)
    expect(milestoneResults[0]).toEqual({
      phase_id: "phase1",
      passed: false,
      reason: MILESTONE_UNVERIFIED_REASON,
    })

    const done = events.filter(e => e.type === "done")
    expect(done).toHaveLength(1)
    expect((done[0] as { status: string }).status).toBe("milestone_pending")

    const logged = await sessionLog.read("ms-leak")
    expect(logged.some(e => e.event.kind === "milestone_blocked")).toBe(true)
    expect(logged.some(e => e.event.kind === "milestone_advanced")).toBe(false)
  })
})

describe("R-B28: the main loop cannot busy-wait on an effect it has no branch for", () => {
  let restore = () => {}
  afterEach(() => restore())

  // The suite timeout is the busy-wait detector: before the `else` backstop this run never returned.
  it("terminates the run with an explicit error", async () => {
    restore = scriptKernel(event => {
      // `preempt_sub_agents` is only ever driven inside the workflow driver — arriving at the
      // main-loop position is precisely the protocol mismatch the backstop must catch.
      if (event.kind === "start_run") {
        return reply([{ kind: "preempt_sub_agents", effect_id: "pre-1", agent_ids: ["ghost"], reason: "test" }])
      }
      return undefined
    })

    const sessionLog = new InMemorySessionLog()
    const events: StreamEvent[] = []
    for await (const evt of makeRunner(sessionLog).run({ sessionId: "unhandled-effect", goal: "test" })) {
      events.push(evt)
    }

    const errors = events.filter(e => e.type === "error") as Array<{ message: string }>
    expect(errors).toHaveLength(1)
    expect(errors[0].message).toContain("unhandled kernel effect preempt_sub_agents")

    const done = events.filter(e => e.type === "done")
    expect(done).toHaveLength(1)
    expect((done[0] as { status: string }).status).toBe("error")

    const logged = await sessionLog.read("unhandled-effect")
    const terminal = logged.map(e => e.event).find(e => e.kind === "run_terminal") as { reason: string } | undefined
    expect(terminal?.reason).toBe("error")
  }, 15_000)
})

describe("R-B32: the default result spool survives long enough to be read back", () => {
  let restore = () => {}
  afterEach(() => restore())

  it("read_result resolves a spool_ref written with no host-configured resultSpool", async () => {
    const huge = "z".repeat(80 * 1024)
    let spoolRef: string | undefined
    restore = scriptKernel(event => {
      if (event.kind === "provider_result" && spoolRef === undefined) {
        return reply([{
          kind: "spool_large_result",
          effect_id: "sp-1",
          call_id: "big-1",
          tool: "big_out",
          output: huge,
          original_size: huge.length,
          preview_size: 512,
        }])
      }
      if (event.kind === "large_result_spool_result") {
        spoolRef = event.spool_ref as string | undefined
        // The model now asks for the evicted output back by call_id.
        return reply([{
          kind: "execute_tool",
          effect_id: "t-1",
          calls: [{ id: "read-1", name: "read_result", arguments: { call_id: "big-1" } }],
        }])
      }
      return undefined
    })

    const sessionLog = new InMemorySessionLog()
    const events: StreamEvent[] = []
    // No `resultSpool` option — the runner's own default spool is the only place the bytes live
    // (the session log never saw a `tool_completed` for `big-1`, so the fallback scan cannot help).
    for await (const evt of makeRunner(sessionLog).run({ sessionId: "spool-default", goal: "test" })) {
      events.push(evt)
    }

    expect(spoolRef).toBeTruthy()
    const read = events.find(e => e.type === "tool_result" && (e as { callId: string }).callId === "read-1") as
      | { content: string; isError: boolean }
      | undefined
    expect(read).toBeDefined()
    expect(read!.isError).toBe(false)
    expect(read!.content).toContain(`of ${huge.length}`)
    expect(read!.content).toContain(huge.slice(0, 1000))
  })
})
