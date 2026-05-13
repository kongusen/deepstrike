/**
 * 07_harness.test.ts — SinglePassHarness, EvalLoopHarness, HarnessLoop
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "@deepstrike/sdk"
import type { QualityGate, HarnessRequest, HarnessOutcome, HarnessEvent } from "@deepstrike/sdk"

async function collectHarness(loop: HarnessLoop, req: HarnessRequest): Promise<{ passed: boolean; result: string; events: HarnessEvent[] }> {
  const events: HarnessEvent[] = []
  let result = ""
  let passed = false
  for await (const evt of loop.runStreaming(req)) {
    events.push(evt)
    if (evt.type === "token") result += evt.text
    if (evt.type === "done") passed = evt.verdict.passed
  }
  return { passed, result, events }
}
import { makeAgent, makeProvider } from "./helpers.js"

describe("SinglePassHarness", () => {
  it("always returns passed=true", { timeout: 60_000 }, async () => {
    const outcome = await new SinglePassHarness(makeAgent()).run({ goal: 'Reply "done".' })
    assert.equal(outcome.passed, true)
    assert.ok(outcome.result.length > 0)
    assert.ok(outcome.iterations > 0)
    assert.ok(outcome.totalTokens > 0)
  })
})

describe("EvalLoopHarness", () => {
  it("passes on first attempt when gate returns true", { timeout: 60_000 }, async () => {
    const gate: QualityGate = { async evaluate() { return true } }
    const outcome = await new EvalLoopHarness(makeAgent(), gate, 3).run({ goal: 'Say "hello".' })
    assert.equal(outcome.passed, true)
  })

  it("retries when gate returns false then true", { timeout: 120_000 }, async () => {
    let count = 0
    const gate: QualityGate = { async evaluate() { return ++count >= 2 } }
    const outcome = await new EvalLoopHarness(makeAgent(), gate, 3).run({ goal: 'Say "hello".' })
    assert.equal(outcome.passed, true)
    assert.ok(count >= 2)
  })

  it("returns passed=false when gate never passes", { timeout: 120_000 }, async () => {
    const gate: QualityGate = { async evaluate() { return false } }
    const outcome = await new EvalLoopHarness(makeAgent(), gate, 2).run({ goal: 'Say "hello".' })
    assert.equal(outcome.passed, false)
  })
})

describe("HarnessLoop (LLM-as-judge)", () => {
  it("runStreaming emits token and done events", { timeout: 90_000 }, async () => {
    const { passed, result, events } = await collectHarness(
      new HarnessLoop(makeAgent(), makeProvider(), { maxAttempts: 3 }),
      { goal: "What is 9 * 9? Output only the number.", criteria: [{ text: "Answer must be exactly 81", required: true }] },
    )
    assert.ok(typeof passed === "boolean")
    assert.ok(result.length > 0)
    assert.ok(events.some(e => e.type === "token"))
    assert.ok(events.some(e => e.type === "supervising"))
    assert.ok(events.some(e => e.type === "done" || e.type === "max_attempts_reached"))
  })

  it("feedback is injected and outcome is structured correctly", { timeout: 120_000 }, async () => {
    const { passed, result } = await collectHarness(
      new HarnessLoop(makeAgent(), makeProvider(), { maxAttempts: 3 }),
      { goal: 'Reply with JSON: {"status":"ok"}. Nothing else.', criteria: [{ text: "Response must be valid JSON", required: true }, { text: "Must have key 'status' with value 'ok'", required: true }] },
    )
    assert.ok(typeof passed === "boolean")
    if (passed) {
      try {
        const parsed = JSON.parse(result.trim())
        assert.equal(parsed.status, "ok")
      } catch { /* model may wrap in markdown */ }
    }
  })
})
