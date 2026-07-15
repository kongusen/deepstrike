/** 07_harness.test.ts — AttemptLoop body/judge/carry/stop contract. */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import {
  AttemptLoop,
  LlmEvalJudge,
  RuntimeAttemptBody,
  VerdictFnJudge,
  type Verdict,
} from "@deepstrike/sdk/harness"
import { makeAgent, makeProvider } from "./helpers.js"

const verdict = (passed: boolean, feedback = "ok"): Verdict => ({
  passed,
  overallScore: passed ? 1 : 0,
  feedback,
  details: [],
})

describe("AttemptLoop", () => {
  it("returns the real result and independent verdict", { timeout: 60_000 }, async () => {
    const outcome = await new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new VerdictFnJudge(() => verdict(true)),
      stop: { maxAttempts: 1 },
    }).run({ goal: 'Reply "done".' })
    assert.equal(outcome.outcome, "passed")
    assert.ok(outcome.result.length > 0)
    assert.ok(outcome.turns > 0)
  })

  it("retries then passes", { timeout: 120_000 }, async () => {
    let attempts = 0
    const outcome = await new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new VerdictFnJudge(() => verdict(++attempts >= 2, "retry")),
      stop: { maxAttempts: 3 },
    }).run({ sessionId: "stable", goal: 'Say "hello".' })
    assert.equal(outcome.outcome, "passed")
    assert.equal(outcome.attempts, 2)
  })

  it("exhaustion is distinct from body run health", { timeout: 120_000 }, async () => {
    const outcome = await new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new VerdictFnJudge(() => verdict(false, "no")),
      stop: { maxAttempts: 2 },
    }).run({ goal: 'Say "hello".' })
    assert.equal(outcome.outcome, "exhausted")
    assert.notEqual(outcome.runStatus, "error")
    assert.equal(outcome.verdict?.passed, false)
  })

  it("stream emits token, judging, and terminal events", { timeout: 120_000 }, async () => {
    const events = []
    let result = ""
    const loop = new AttemptLoop({
      body: new RuntimeAttemptBody(makeAgent().runner),
      judge: new LlmEvalJudge(makeProvider()),
      stop: { maxAttempts: 2 },
    })
    for await (const event of loop.stream({
      goal: "What is 9 * 9? Output only the number.",
      criteria: [{ text: "Answer must be exactly 81", required: true }],
    })) {
      events.push(event)
      if (event.type === "token") result += event.text
    }
    assert.ok(result.length > 0)
    assert.ok(events.some(event => event.type === "judging"))
    assert.ok(events.some(event => event.type === "completed"))
  })
})
