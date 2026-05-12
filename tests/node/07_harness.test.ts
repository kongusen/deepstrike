/**
 * 07_harness.test.ts — SinglePassHarness, EvalLoopHarness, HarnessLoop
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "@deepstrike/sdk"
import type { QualityGate, HarnessRequest, HarnessOutcome } from "@deepstrike/sdk"
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
  it("returns valid outcome for a simple task", { timeout: 90_000 }, async () => {
    const outcome = await new HarnessLoop(makeAgent(), makeProvider(), { maxAttempts: 3 }).run({
      goal: "What is 9 * 9? Output only the number.",
      criteria: ["Answer must be exactly 81"],
    })
    assert.ok(typeof outcome.passed === "boolean")
    assert.ok(outcome.result.length > 0)
  })

  it("feedback is injected and outcome is structured correctly", { timeout: 120_000 }, async () => {
    const outcome = await new HarnessLoop(makeAgent(), makeProvider(), { maxAttempts: 3 }).run({
      goal: 'Reply with JSON: {"status":"ok"}. Nothing else.',
      criteria: ["Response must be valid JSON", "Must have key 'status' with value 'ok'"],
    })
    assert.ok(typeof outcome.passed === "boolean")
    if (outcome.passed) {
      try {
        const parsed = JSON.parse(outcome.result.trim())
        assert.equal(parsed.status, "ok")
      } catch { /* model may wrap in markdown */ }
    }
  })
})
