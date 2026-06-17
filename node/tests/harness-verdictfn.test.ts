/**
 * I3.2 regression tests for HarnessLoop.verdictFn (A2/A3).
 *
 * Four cases:
 *  1. Default — no verdictFn ⇒ behavior is byte-equivalent to the prior LLM-eval path.
 *  2. Pass short-circuit — verdictFn returns {passed:true} ⇒ no evalProvider.stream call,
 *     loop terminates with done + the host's Verdict.
 *  3. Fail short-circuit — verdictFn returns {passed:false} ⇒ no evalProvider.stream call,
 *     loop emits `revising` and threads the feedback into the next attempt's goal.
 *  4. Defer (undefined) — verdictFn returns undefined ⇒ fall through to the built-in eval.
 */

import { jest, describe, it, expect, beforeEach } from "@jest/globals"
import { HarnessLoop, type VerdictFn, type Verdict } from "../src/harness/harness.js"
import type { RuntimeRunner } from "../src/runtime/runner.js"
import type { LLMProvider, RenderedContext, StreamEvent, ToolSchema, ProviderRunState, Message } from "../src/types.js"

// ── mocks ──────────────────────────────────────────────────────────────────

function makeFakeRunner(text: string): RuntimeRunner {
  // Yields a single text_delta then done. The HarnessLoop reads these as the attempt result.
  return {
    run() {
      return (async function*() {
        yield { type: "text_delta", delta: text } as StreamEvent
        yield { type: "done", iterations: 1, totalTokens: 100, status: "completed" } as StreamEvent
      })()
    },
  } as unknown as RuntimeRunner
}

function makeFakeEvalProvider(verdictJson: string): { provider: LLMProvider; streamCalls: number } {
  const captured = { streamCalls: 0 }
  const provider: LLMProvider = {
    async complete(_ctx: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
      return { role: "assistant", content: verdictJson }
    },
    async *stream(_ctx: RenderedContext, _tools: ToolSchema[], _ext?: Record<string, unknown>, _state?: ProviderRunState): AsyncIterable<StreamEvent> {
      captured.streamCalls++
      yield { type: "text_delta", delta: verdictJson } as StreamEvent
      yield { type: "done", iterations: 1, totalTokens: 10, status: "completed" } as StreamEvent
    },
  }
  return { provider, streamCalls: captured.streamCalls, get } as unknown as { provider: LLMProvider; streamCalls: number }

  function get() { return captured.streamCalls }
}

// More direct: build a tracked eval provider.
function trackedEvalProvider(verdictJson: string) {
  const state = { streamCalls: 0 }
  const provider: LLMProvider = {
    async complete(): Promise<Message> { return { role: "assistant", content: verdictJson } },
    async *stream() {
      state.streamCalls++
      yield { type: "text_delta", delta: verdictJson } as StreamEvent
      yield { type: "done", iterations: 1, totalTokens: 10, status: "completed" } as StreamEvent
    },
  }
  return { provider, state }
}

// ── tests ──────────────────────────────────────────────────────────────────

describe("HarnessLoop verdictFn", () => {
  const passingEvalJson = JSON.stringify({
    passed: true,
    overall_score: 1,
    feedback: "looks good",
    details: [{ criterion: "c1", passed: true, score: 1, feedback: "ok" }],
  })

  it("default: no verdictFn ⇒ evalProvider.stream is called and the LLM verdict drives done/revising", async () => {
    const runner = makeFakeRunner("attempt 1 output")
    const { provider: evalProvider, state } = trackedEvalProvider(passingEvalJson)
    const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 1 })
    const events: any[] = []
    for await (const evt of loop.stream({ goal: "g", criteria: [{ text: "c1", required: true }] })) {
      events.push(evt)
    }
    expect(state.streamCalls).toBe(1)
    const done = events.find(e => e.type === "done")
    expect(done).toBeDefined()
    expect(done.verdict.passed).toBe(true)
  })

  it("passing verdictFn short-circuits: evalProvider.stream is NOT called", async () => {
    const runner = makeFakeRunner("attempt 1 output")
    const { provider: evalProvider, state } = trackedEvalProvider(passingEvalJson)
    const verdictFn: VerdictFn = ({ result }) => ({
      passed: true,
      overallScore: 1,
      feedback: `host approved: ${result.slice(0, 20)}`,
      details: [],
    })
    const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 1, verdictFn })
    const events: any[] = []
    for await (const evt of loop.stream({ goal: "g", criteria: [{ text: "c1", required: true }] })) {
      events.push(evt)
    }
    expect(state.streamCalls).toBe(0)
    const done = events.find(e => e.type === "done")
    expect(done).toBeDefined()
    expect(done.verdict.passed).toBe(true)
    expect(done.verdict.feedback).toContain("host approved")
  })

  it("failing verdictFn yields revising + threads feedback into next attempt's goal", async () => {
    // Two attempts: both fail the host check; collect revising events + feedback text.
    let attemptCount = 0
    const observedGoals: string[] = []
    const runner = {
      run(req: { goal: string }) {
        observedGoals.push(req.goal)
        return (async function*() {
          attemptCount++
          yield { type: "text_delta", delta: `output-${attemptCount}` } as StreamEvent
          yield { type: "done", iterations: 1, totalTokens: 10, status: "completed" } as StreamEvent
        })()
      },
    } as unknown as RuntimeRunner
    const { provider: evalProvider, state } = trackedEvalProvider(passingEvalJson)
    let verdictCalls = 0
    const verdictFn: VerdictFn = ({ attempt }): Verdict => {
      verdictCalls++
      return {
        passed: false,
        overallScore: 0,
        feedback: `host rejects attempt ${attempt}`,
        details: [],
      }
    }
    const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 2, verdictFn })
    const events: any[] = []
    for await (const evt of loop.stream({ goal: "g0", criteria: [{ text: "c1", required: true }] })) {
      events.push(evt)
    }
    expect(state.streamCalls).toBe(0) // never called
    expect(verdictCalls).toBe(2) // one per attempt
    const revising = events.filter(e => e.type === "revising")
    expect(revising.length).toBe(2)
    expect(revising[0].verdict.passed).toBe(false)
    expect(revising[0].verdict.feedback).toBe("host rejects attempt 1")
    // The second attempt's goal must carry the prior attempt's feedback.
    expect(observedGoals[1]).toContain("host rejects attempt 1")
    // No done event when max attempts is reached.
    const max = events.find(e => e.type === "max_attempts_reached")
    expect(max).toBeDefined()
  })

  it("verdictFn returning undefined falls back to the built-in LLM eval", async () => {
    const runner = makeFakeRunner("attempt 1 output")
    const { provider: evalProvider, state } = trackedEvalProvider(passingEvalJson)
    let verdictCalls = 0
    const verdictFn: VerdictFn = () => {
      verdictCalls++
      return undefined // defer
    }
    const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 1, verdictFn })
    const events: any[] = []
    for await (const evt of loop.stream({ goal: "g", criteria: [{ text: "c1", required: true }] })) {
      events.push(evt)
    }
    expect(verdictCalls).toBe(1)
    expect(state.streamCalls).toBe(1) // fell through to eval
    const done = events.find(e => e.type === "done")
    expect(done).toBeDefined()
    expect(done.verdict.passed).toBe(true) // came from LLM eval JSON, not host
  })
})
