import { InMemoryDreamStore } from "../src/memory/in-memory-store.js"
import type { MemoryEntry, SessionData } from "../src/memory/protocols.js"

const session = (agentId: string, sessionId = "s1"): SessionData => ({
  sessionId,
  agentId,
  messages: [],
  metadata: null,
  createdAtMs: 1,
  updatedAtMs: 1,
})

describe("InMemoryDreamStore", () => {
  it("returns empty arrays for an unknown agent", async () => {
    const store = new InMemoryDreamStore()
    expect(await store.loadSessions("agent-x")).toEqual([])
    expect(await store.loadMemories("agent-x")).toEqual([])
  })

  it("addSession + loadSessions round-trip", async () => {
    const store = new InMemoryDreamStore()
    store.addSession("a1", session("a1"))
    expect((await store.loadSessions("a1")).length).toBe(1)
  })

  it("seeds initial memories on first loadMemories per agent", async () => {
    const seed: MemoryEntry[] = [{ text: "fact-a", score: 0.9, metadata: null }]
    const store = new InMemoryDreamStore(seed)
    expect((await store.loadMemories("a1")).map(m => m.text)).toEqual(["fact-a"])
    // Subsequent calls return the same set (no re-seed).
    expect((await store.loadMemories("a1")).length).toBe(1)
    // A different agent gets its own seed.
    expect((await store.loadMemories("a2")).map(m => m.text)).toEqual(["fact-a"])
  })

  it("addMemories appends to existing entries", async () => {
    const store = new InMemoryDreamStore()
    store.addMemories("a1", [{ text: "x", score: 0.5, metadata: null }])
    store.addMemories("a1", [{ text: "y", score: 0.5, metadata: null }])
    expect((await store.loadMemories("a1")).map(m => m.text)).toEqual(["x", "y"])
  })

  it("commit removes indices then appends", async () => {
    const store = new InMemoryDreamStore()
    const existing: MemoryEntry[] = [
      { text: "a", score: 0.5, metadata: null },
      { text: "b", score: 0.5, metadata: null },
    ]
    await store.commit("a1", {
      toAdd: [{ text: "c", score: 0.9, metadata: null }],
      toRemoveIndices: [0],
      stats: { insightsProcessed: 1, duplicatesRemoved: 0, conflictsResolved: 0, entriesAdded: 1 },
    }, existing)
    expect((await store.loadMemories("a1")).map(m => m.text)).toEqual(["b", "c"])
  })

  it("search returns up to topK in insertion order", async () => {
    const store = new InMemoryDreamStore([
      { text: "one", score: 0.1, metadata: null },
      { text: "two", score: 0.2, metadata: null },
      { text: "three", score: 0.3, metadata: null },
    ])
    expect((await store.search("a1", "anything", 2)).map(m => m.text)).toEqual(["one", "two"])
  })

  it("saveSession persists into both savedSessions and the sessions map", async () => {
    const store = new InMemoryDreamStore()
    await store.saveSession(session("a1", "s-saved"))
    expect(store.savedSessions.length).toBe(1)
    expect((await store.loadSessions("a1")).map(s => s.sessionId)).toEqual(["s-saved"])
  })
})
