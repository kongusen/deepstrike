import {
  AttemptLoop,
  RuntimeAttemptBody,
  freshWithFeedback,
  type AttemptOutcome,
  type VerdictFn,
} from "../src/harness/harness.js"
import { VerdictFnJudge } from "../src/harness/judge.js"
import type { RuntimeRunner } from "../src/runtime/runner.js"
import type { StreamEvent } from "../src/types.js"
import { attemptOutcomeToLoopResult } from "../src/runtime/sub-agent-orchestrator.js"
import { workflowNodeStatusFromTermination } from "../src/types/agent.js"

interface ScriptedRun {
  text: string
  status: string
  turns: number
  tokens: number
}

function scriptedRunner(script: ScriptedRun[]) {
  const runs: Array<{ sessionId: string; goal: string }> = []
  const notes: string[] = []
  let index = 0
  const runner = {
    injectNote(text: string) {
      notes.push(text)
    },
    run(request: { sessionId: string; goal: string }) {
      runs.push(request)
      const current = script[index++]!
      return (async function*() {
        yield { type: "text_delta", delta: current.text } as StreamEvent
        yield {
          type: "done",
          iterations: current.turns,
          totalTokens: current.tokens,
          status: current.status,
        } as StreamEvent
      })()
    },
  } as unknown as RuntimeRunner
  return { runner, runs, notes }
}

const verdict = (passed: boolean, feedback: string) => ({
  passed,
  overallScore: passed ? 1 : 0,
  feedback,
  details: [],
})

describe("AttemptLoop", () => {
  it("defaults to continueSession and injects feedback without rewriting the goal", async () => {
    const { runner, runs, notes } = scriptedRunner([
      { text: "first", status: "completed", turns: 1, tokens: 10 },
      { text: "second", status: "completed", turns: 2, tokens: 20 },
    ])
    const judge = new VerdictFnJudge(({ attempt }) =>
      verdict(attempt === 2, attempt === 1 ? "fix the failing assertion" : "ok"))
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge,
      stop: { maxAttempts: 2 },
    })

    const outcome = await loop.run({ sessionId: "stable-session", goal: "original goal" })

    expect(runs).toEqual([
      { sessionId: "stable-session", goal: "original goal", criteria: [], extensions: undefined },
      { sessionId: "stable-session", goal: "original goal", criteria: [], extensions: undefined },
    ])
    expect(notes).toEqual(["fix the failing assertion"])
    expect(outcome).toMatchObject({
      outcome: "passed",
      result: "second",
      attempts: 2,
      turns: 3,
      totalTokens: 30,
      runStatus: "completed",
      verdict: { passed: true },
    })
  })

  it("does not judge a run error", async () => {
    const { runner } = scriptedRunner([
      { text: "partial", status: "error", turns: 1, tokens: 7 },
    ])
    let judgeCalls = 0
    const verdictFn: VerdictFn = () => {
      judgeCalls++
      return verdict(true, "must not be used")
    }
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(verdictFn),
      stop: { maxAttempts: 3 },
    })

    const outcome = await loop.run({ sessionId: "failed-session", goal: "g" })

    expect(judgeCalls).toBe(0)
    expect(outcome).toEqual({
      outcome: "run_error",
      runStatus: "error",
      result: "partial",
      attempts: 1,
      turns: 1,
      totalTokens: 7,
    })
  })

  it("returns the final result and cumulative usage when attempts are exhausted", async () => {
    const { runner } = scriptedRunner([
      { text: "draft", status: "completed", turns: 2, tokens: 11 },
      { text: "final draft", status: "completed", turns: 3, tokens: 13 },
    ])
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(() => verdict(false, "still incomplete")),
      stop: { maxAttempts: 2 },
    })

    const outcome: AttemptOutcome = await loop.run({ sessionId: "exhausted", goal: "g" })

    expect(outcome).toMatchObject({
      outcome: "exhausted",
      result: "final draft",
      attempts: 2,
      turns: 5,
      totalTokens: 24,
      runStatus: "completed",
      verdict: { passed: false, feedback: "still incomplete" },
    })
  })

  it("keeps a one-shot failed verdict healthy at the sub-agent boundary", async () => {
    const { runner } = scriptedRunner([
      { text: "valid run, insufficient answer", status: "completed", turns: 1, tokens: 9 },
    ])
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(() => verdict(false, "criterion failed")),
      stop: { maxAttempts: 3, stopOnFailedVerdict: true },
    })

    const outcome = await loop.run({ sessionId: "one-shot", goal: "g" })
    const result = attemptOutcomeToLoopResult(outcome)

    expect(outcome.outcome).toBe("failed_judge")
    expect(outcome.runStatus).toBe("completed")
    expect(outcome.verdict?.passed).toBe(false)
    expect(result.termination).toBe("completed")
    expect(workflowNodeStatusFromTermination(result.termination)).toBe("completed")
  })

  it("forwards attachments to every attempt (same-session carry)", async () => {
    const attachments = [{ type: "image" as const, data: "iVBORw0KGgo=", mediaType: "image/png" }]
    const { runner, runs } = scriptedRunner([
      { text: "first", status: "completed", turns: 1, tokens: 1 },
      { text: "second", status: "completed", turns: 1, tokens: 1 },
    ])
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(({ attempt }) => verdict(attempt === 2, "retry")),
      stop: { maxAttempts: 2 },
    })

    await loop.run({ sessionId: "with-image", goal: "describe", attachments })

    expect(runs).toHaveLength(2)
    for (const request of runs as Array<{ attachments?: unknown }>) {
      expect(request.attachments).toEqual(attachments)
    }
  })

  it("re-seeds attachments on fresh-session retries (the silent-loss case)", async () => {
    const attachments = [{ type: "image" as const, data: "iVBORw0KGgo=", mediaType: "image/png" }]
    const { runner, runs } = scriptedRunner([
      { text: "first", status: "completed", turns: 1, tokens: 1 },
      { text: "second", status: "completed", turns: 1, tokens: 1 },
    ])
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(({ attempt }) => verdict(attempt === 2, "needs another pass")),
      carry: freshWithFeedback,
      stop: { maxAttempts: 2 },
    })

    await loop.run({ sessionId: "fresh-image", goal: "describe", attachments })

    expect(runs).toHaveLength(2)
    expect((runs[0] as { sessionId: string }).sessionId)
      .not.toBe((runs[1] as { sessionId: string }).sessionId)
    // Attempt 2 runs in a brand-new session: without forwarding it would silently lose the image.
    expect((runs[1] as { attachments?: unknown }).attachments).toEqual(attachments)
  })

  it("keeps fresh-session goal concatenation behind an explicit carry policy", async () => {
    const { runner, runs, notes } = scriptedRunner([
      { text: "first", status: "completed", turns: 1, tokens: 1 },
      { text: "second", status: "completed", turns: 1, tokens: 1 },
    ])
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(runner),
      judge: new VerdictFnJudge(({ attempt }) => verdict(attempt === 2, "explicit feedback")),
      carry: freshWithFeedback,
      stop: { maxAttempts: 2 },
    })

    await loop.run({ sessionId: "root", goal: "g" })

    expect(runs[0]!.sessionId).not.toBe(runs[1]!.sessionId)
    expect(runs[1]!.goal).toContain("explicit feedback")
    expect(notes).toEqual([])
  })

  it("keeps run health and verdict metadata separate at the sub-agent boundary", () => {
    const outcome: AttemptOutcome = {
      outcome: "exhausted",
      runStatus: "completed",
      verdict: verdict(false, "criteria failed"),
      result: "healthy run output",
      attempts: 2,
      turns: 4,
      totalTokens: 40,
    }

    expect(attemptOutcomeToLoopResult(outcome)).toEqual({
      termination: "error",
      finalMessage: { role: "assistant", content: "healthy run output", toolCalls: [] },
      turnsUsed: 4,
      totalTokensUsed: 40,
      attempt: {
        outcome: "exhausted",
        runStatus: "completed",
        attempts: 2,
        verdict: verdict(false, "criteria failed"),
      },
    })
  })

  it("maps the two axes without turning failed_judge into a run error", () => {
    const cases: Array<{
      outcome: AttemptOutcome
      termination: string
      workflowStatus: string
    }> = [
      {
        outcome: {
          outcome: "passed",
          runStatus: "completed",
          verdict: verdict(true, "ok"),
          result: "accepted",
          attempts: 1,
          turns: 1,
          totalTokens: 10,
        },
        termination: "completed",
        workflowStatus: "completed",
      },
      {
        outcome: {
          outcome: "failed_judge",
          runStatus: "completed",
          verdict: verdict(false, "criterion failed"),
          result: "healthy but rejected",
          attempts: 1,
          turns: 1,
          totalTokens: 10,
        },
        termination: "completed",
        workflowStatus: "completed",
      },
      {
        outcome: {
          outcome: "exhausted",
          runStatus: "completed",
          verdict: verdict(false, "still failing"),
          result: "last attempt",
          attempts: 3,
          turns: 3,
          totalTokens: 30,
        },
        termination: "error",
        workflowStatus: "failed",
      },
      {
        outcome: {
          outcome: "run_error",
          runStatus: "error",
          result: "partial",
          attempts: 1,
          turns: 1,
          totalTokens: 5,
        },
        termination: "error",
        workflowStatus: "failed",
      },
    ]

    for (const testCase of cases) {
      const result = attemptOutcomeToLoopResult(testCase.outcome)
      expect(result.termination).toBe(testCase.termination)
      expect(workflowNodeStatusFromTermination(result.termination)).toBe(testCase.workflowStatus)
      expect(result.attempt?.runStatus).toBe(testCase.outcome.runStatus)
      expect(result.attempt?.verdict).toEqual(testCase.outcome.verdict)
    }
  })
})
