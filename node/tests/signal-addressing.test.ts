/**
 * R1 / L0 — recipient addressing on a shared SignalGateway.
 *
 * One gateway serves N peer loops: each loop pulls with its own sessionId and drains only
 * signals addressed to it (plus unaddressed shared signals); other recipients' signals stay
 * queued. Omitting the recipient preserves the legacy FIFO behaviour byte-for-byte.
 */
import { SignalGateway } from "../src/os/public.js"
import type { RuntimeSignal } from "../src/index.js"

const sig = (summary: string, recipient?: string): RuntimeSignal => ({
  source: "gateway",
  signalType: "event",
  urgency: "normal",
  payload: { goal: summary },
  ...(recipient ? { recipient } : {}),
})

describe("SignalGateway recipient addressing (R1/L0)", () => {
  it("redelivers an unacked claim after its lease expires", async () => {
    let now = 1_000
    const gw = new SignalGateway({ now: () => now, defaultLeaseMs: 100 })
    gw.ingest(sig("leased", "sess-a"))

    const first = await gw.claimSignal("sess-a")
    expect(first?.signal.payload.goal).toBe("leased")
    expect(await gw.claimSignal("sess-a")).toBeNull()

    now += 101
    const second = await gw.claimSignal("sess-a")
    expect(second?.signal.payload.goal).toBe("leased")
    expect(second?.leaseToken).not.toBe(first?.leaseToken)
    expect(await gw.ackSignal(first!)).toBe(false)
    expect(await gw.ackSignal(second!)).toBe(true)
    expect(gw.depth).toBe(0)
  })

  it("makes a nacked claim immediately available for redelivery", async () => {
    const gw = new SignalGateway()
    gw.ingest(sig("retry", "sess-a"))

    const first = await gw.claimSignal("sess-a")
    expect(await gw.nackSignal(first!)).toBe(true)
    const second = await gw.claimSignal("sess-a")

    expect(second?.signal.payload.goal).toBe("retry")
    expect(second?.leaseToken).not.toBe(first?.leaseToken)
  })

  it("each loop drains only its own + shared signals from one shared gateway", async () => {
    const gw = new SignalGateway()
    gw.ingest(sig("to-a", "sess-a"))
    gw.ingest(sig("to-b", "sess-b"))
    gw.ingest(sig("shared"))

    // sess-a sees its own then the shared item, never sess-b's.
    const a1 = await gw.nextSignal("sess-a")
    const a2 = await gw.nextSignal("sess-a")
    expect([a1?.payload.goal, a2?.payload.goal].sort()).toEqual(["shared", "to-a"])
    expect(await gw.nextSignal("sess-a")).toBeNull()

    // sess-b's signal is still queued for its own puller.
    expect((await gw.nextSignal("sess-b"))?.payload.goal).toBe("to-b")
  })

  it("preserves FIFO order among a recipient's visible signals", async () => {
    const gw = new SignalGateway()
    gw.ingest(sig("first", "sess-a"))
    gw.ingest(sig("to-b", "sess-b"))
    gw.ingest(sig("second")) // broadcast, after to-b
    expect((await gw.nextSignal("sess-a"))?.payload.goal).toBe("first")
    expect((await gw.nextSignal("sess-a"))?.payload.goal).toBe("second")
  })

  it("omitting recipient is legacy FIFO drain (any signal, in order)", async () => {
    const gw = new SignalGateway()
    gw.ingest(sig("x", "sess-a"))
    gw.ingest(sig("y"))
    expect((await gw.nextSignal())?.payload.goal).toBe("x")
    expect((await gw.nextSignal())?.payload.goal).toBe("y")
    expect(await gw.nextSignal()).toBeNull()
  })

  it("fans a broadcast out so every explicit recipient receives one copy", async () => {
    const gw = new SignalGateway()

    gw.broadcast(["sess-a", "sess-b"], sig("all"))

    expect((await gw.nextSignal("sess-a"))?.payload.goal).toBe("all")
    expect((await gw.nextSignal("sess-b"))?.payload.goal).toBe("all")
  })

  it("does not turn a committed ingest into a failure when an observer throws", () => {
    const failures: string[] = []
    const gw = new SignalGateway({
      onObserverError: failure => failures.push(`${failure.operation}:${String(failure.cause)}`),
    })
    gw.onSignal(() => { throw new Error("observer unavailable") })

    expect(() => gw.ingest(sig("committed"))).not.toThrow()
    expect(gw.depth).toBe(1)
    expect(failures[0]).toContain("signal_listener")
  })
})
