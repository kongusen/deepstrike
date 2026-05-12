/**
 * 02_agent.test.ts — Agent.run(), runStreaming(), telemetry, interrupt
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import type { DoneEvent, StreamEvent } from "@deepstrike/sdk"
import { makeAgent, collectEvents, text } from "./helpers.js"

describe("Agent.run()", () => {
  it("returns a non-empty text string", { timeout: 60_000 }, async () => {
    const result = await makeAgent().run('Reply with the single word "pong".')
    assert.ok(result.length > 0)
    assert.ok(result.toLowerCase().includes("pong"), `got: ${result}`)
  })

  it("arithmetic task returns correct number", { timeout: 60_000 }, async () => {
    const result = await makeAgent().run("What is 7 * 8? Output only the number.")
    assert.ok(result.includes("56"), `got: ${result}`)
  })
})

describe("Agent.runStreaming()", () => {
  it("emits text_delta and done events", { timeout: 60_000 }, async () => {
    const events = await collectEvents(makeAgent().runStreaming('Say "hi"'))
    assert.ok(events.some(e => e.type === "text_delta"), "need text_delta")
    assert.equal(events.filter(e => e.type === "done").length, 1, "need exactly 1 done")
  })

  it("done event has positive token and iteration counts", { timeout: 60_000 }, async () => {
    const events = await collectEvents(makeAgent().runStreaming("Compute 3+4 and output the result."))
    const done = events.find(e => e.type === "done") as DoneEvent
    assert.ok(done.totalTokens > 0)
    assert.ok(done.iterations > 0)
  })

  it("done event status is a known value", { timeout: 60_000 }, async () => {
    const events = await collectEvents(makeAgent().runStreaming("Reply OK"))
    const done = events.find(e => e.type === "done") as DoneEvent
    assert.ok(["success", "max_turns", "timeout", "error"].includes(done.status))
  })

  it("collected text matches the answer", { timeout: 60_000 }, async () => {
    const events = await collectEvents(makeAgent().runStreaming('Say exactly "deepstrike"'))
    assert.ok(text(events).toLowerCase().includes("deepstrike"))
  })

  it("supports criteria list", { timeout: 60_000 }, async () => {
    const result = await makeAgent().run(
      "List two colors.",
      ["Response must mention 'red'", "Response must mention 'blue'"],
    )
    assert.ok(result.toLowerCase().includes("red"))
    assert.ok(result.toLowerCase().includes("blue"))
  })
})

describe("Agent telemetry", () => {
  it("turn starts at 0 before a run", () => {
    const agent = makeAgent()
    assert.equal(agent.turn, 0)
  })

  it("pressure is in [0,1] after a run", { timeout: 60_000 }, async () => {
    const agent = makeAgent()
    await agent.run("Reply OK")
    assert.ok(agent.pressure >= 0 && agent.pressure <= 1, `pressure: ${agent.pressure}`)
  })
})

describe("Agent.interrupt()", () => {
  it("stops the run and still emits a done event", { timeout: 60_000 }, async () => {
    const agent = makeAgent({ maxTurns: 50 })
    const events: StreamEvent[] = []

    for await (const evt of agent.runStreaming("Count from 1 to 1000, one number per sentence.")) {
      events.push(evt)
      if (events.length >= 3) agent.interrupt()
    }

    const done = events.find(e => e.type === "done") as DoneEvent | undefined
    assert.ok(done, "done must be emitted after interrupt")
  })
})
