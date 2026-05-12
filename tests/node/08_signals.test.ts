/**
 * 08_signals.test.ts — SignalGateway offline + agent interrupt via signal
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { SignalGateway, ScheduledPrompt } from "@deepstrike/sdk"
import type { DoneEvent, StreamEvent } from "@deepstrike/sdk"
import { makeAgent, collectEvents } from "./helpers.js"

describe("SignalGateway", () => {
  it("nextSignal() returns null when queue is empty", async () => {
    assert.equal(await new SignalGateway().nextSignal(), null)
  })

  it("ingest() queues a signal, nextSignal() drains it", async () => {
    const gw = new SignalGateway()
    gw.ingest({ kind: "external", payload: { message: "hello" } })
    const sig = await gw.nextSignal()
    assert.ok(sig !== null)
    assert.equal(sig!.kind, "external")
    assert.equal(await gw.nextSignal(), null)
  })

  it("drains signals in FIFO order", async () => {
    const gw = new SignalGateway()
    for (const order of [1, 2, 3]) gw.ingest({ kind: "external", payload: { order } })
    const results: number[] = []
    let sig = await gw.nextSignal()
    while (sig) { results.push((sig.payload as { order: number }).order); sig = await gw.nextSignal() }
    assert.deepEqual(results, [1, 2, 3])
  })

  it("depth reports current queue size", () => {
    const gw = new SignalGateway()
    assert.equal(gw.depth, 0)
    gw.ingest({ kind: "external", payload: {} })
    gw.ingest({ kind: "external", payload: {} })
    assert.equal(gw.depth, 2)
  })

  it("onSignal() fires synchronously on ingest", () => {
    const gw = new SignalGateway()
    const received: string[] = []
    gw.onSignal(sig => received.push(sig.kind))
    gw.ingest({ kind: "external", payload: {} })
    gw.ingest({ kind: "interrupt", payload: {} })
    assert.deepEqual(received, ["external", "interrupt"])
  })

  it("cancel() prevents a scheduled signal from firing", async () => {
    const gw = new SignalGateway()
    const runAtMs = Date.now() + 50
    gw.schedule(new ScheduledPrompt("test", runAtMs))
    gw.cancel("test", runAtMs)
    await new Promise(r => setTimeout(r, 80))
    assert.equal(gw.depth, 0)
    gw.destroy()
  })

  it("schedule() fires at runAtMs", async () => {
    const gw = new SignalGateway()
    const received: string[] = []
    gw.onSignal(sig => received.push(String(sig.payload.goal)))
    gw.schedule(new ScheduledPrompt("fire-me", Date.now() + 40))
    await new Promise(r => setTimeout(r, 80))
    assert.ok(received.includes("fire-me"))
    gw.destroy()
  })

  it("schedule() is idempotent for same goal+time", async () => {
    const gw = new SignalGateway()
    const received: number[] = []
    gw.onSignal(() => received.push(1))
    const runAtMs = Date.now() + 40
    gw.schedule(new ScheduledPrompt("once", runAtMs))
    gw.schedule(new ScheduledPrompt("once", runAtMs))   // duplicate
    await new Promise(r => setTimeout(r, 80))
    assert.equal(received.length, 1)
    gw.destroy()
  })

  it("destroy() clears all pending timers", async () => {
    const gw = new SignalGateway()
    gw.schedule(new ScheduledPrompt("never", Date.now() + 200))
    gw.destroy()
    await new Promise(r => setTimeout(r, 250))
    assert.equal(gw.depth, 0)
  })
})

describe("Agent interrupt (real API)", () => {
  it("Agent.interrupt() stops run and emits done", { timeout: 60_000 }, async () => {
    const agent = makeAgent({ maxTurns: 50 })
    const events: StreamEvent[] = []
    for await (const evt of agent.runStreaming("Count from 1 to 10000.")) {
      events.push(evt)
      if (!agent["interrupted"]) agent.interrupt()
    }
    assert.ok(events.some(e => e.type === "done"), "done must be emitted")
  })

  it("ingest interrupt signal stops agent run", { timeout: 60_000 }, async () => {
    const gw = new SignalGateway()
    const agent = makeAgent({ signalSource: gw, maxTurns: 50 })
    const events: StreamEvent[] = []
    for await (const evt of agent.runStreaming("Count from 1 to 10000.")) {
      events.push(evt)
      if (events.length === 2) gw.ingest({ kind: "interrupt", payload: {} })
    }
    gw.destroy()
    const done = events.find(e => e.type === "done") as DoneEvent | undefined
    assert.ok(done, "done must be emitted even after interrupt signal")
  })
})
