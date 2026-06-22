/**
 * R1 / L0 — recipient addressing on a shared SignalGateway.
 *
 * One gateway serves N peer loops: each loop pulls with its own sessionId and drains only
 * signals addressed to it (plus unaddressed broadcasts); other recipients' signals stay
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
  it("each loop drains only its own + broadcast signals from one shared gateway", async () => {
    const gw = new SignalGateway()
    gw.ingest(sig("to-a", "sess-a"))
    gw.ingest(sig("to-b", "sess-b"))
    gw.ingest(sig("all")) // broadcast

    // sess-a sees its own then the broadcast, never sess-b's.
    const a1 = await gw.nextSignal("sess-a")
    const a2 = await gw.nextSignal("sess-a")
    expect([a1?.payload.goal, a2?.payload.goal].sort()).toEqual(["all", "to-a"])
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
})
