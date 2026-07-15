import {
  AttemptLoop,
  type AttemptBody,
  type AttemptBodyContext,
  type AttemptBodyEvent,
  type AttemptJudge,
} from "../src/harness/index.js"

describe("AttemptLoop", () => {
  it("defaults to stable-session carry and keeps feedback out of the goal", async () => {
    const contexts: AttemptBodyContext[] = []
    const body: AttemptBody = {
      async *run(context): AsyncIterable<AttemptBodyEvent> {
        contexts.push(context)
        yield {
          type: "body_done",
          runStatus: "completed",
          result: `attempt-${context.attempt}`,
          turns: 1,
          totalTokens: 5,
        }
      },
    }
    const judge: AttemptJudge = {
      async judge(context) {
        return {
          verdict: {
            passed: context.attempt === 2,
            overallScore: context.attempt === 2 ? 1 : 0,
            feedback: context.attempt === 2 ? "ok" : "fix it",
            details: [],
          },
        }
      },
    }

    const outcome = await new AttemptLoop({ body, judge, stop: { maxAttempts: 2 } }).run({
      sessionId: "stable",
      goal: "original",
    })

    expect(outcome.outcome).toBe("passed")
    expect(outcome.totalTokens).toBe(10)
    expect(contexts.map(context => context.sessionId)).toEqual(["stable", "stable"])
    expect(contexts.map(context => context.goal)).toEqual(["original", "original"])
    expect(contexts.map(context => context.contextInput)).toEqual([undefined, "fix it"])
  })

  it("keeps run health separate and skips the judge on a body error", async () => {
    let judgeCalls = 0
    const body: AttemptBody = {
      async *run() {
        yield {
          type: "body_done",
          runStatus: "error",
          result: "partial",
          turns: 1,
          totalTokens: 3,
        }
      },
    }
    const judge: AttemptJudge = {
      async judge() {
        judgeCalls++
        throw new Error("must not run")
      },
    }

    const outcome = await new AttemptLoop({ body, judge, stop: { maxAttempts: 2 } }).run({
      goal: "g",
    })

    expect(outcome.outcome).toBe("run_error")
    expect(outcome.verdict).toBeUndefined()
    expect(outcome.result).toBe("partial")
    expect(judgeCalls).toBe(0)
  })
})
