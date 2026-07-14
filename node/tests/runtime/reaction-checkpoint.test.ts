import { InMemoryReactionCheckpointStore } from "../../src/index.js"

describe("InMemoryReactionCheckpointStore", () => {
  it("allows only one active event worker and rejects stale writes after expiry", async () => {
    let now = 1_000
    const store = new InMemoryReactionCheckpointStore({ now: () => now, defaultLeaseMs: 100 })
    const first = await store.claim("event")
    expect(first.status).toBe("claimed")
    expect((await store.claim("event")).status).toBe("busy")
    if (first.status !== "claimed") throw new Error("expected claim")
    await store.savePlan(first.claim, ["alice"])

    now += 101
    const second = await store.claim("event")
    if (second.status !== "claimed") throw new Error("expected expired claim to be reclaimed")
    expect(await store.record(first.claim, { personaId: "alice", output: "stale" })).toBe(false)
    expect(await store.record(second.claim, { personaId: "alice", output: "fresh" })).toBe(true)
    expect(await store.complete(second.claim)).toBe(true)

    expect(await store.claim("event")).toEqual({
      status: "completed",
      reactions: [{ personaId: "alice", output: "fresh" }],
    })
  })
})
