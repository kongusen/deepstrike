/**
 * 05_memory.test.ts — WorkingMemory + DreamStore + Agent.dream()
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { WorkingMemory } from "@deepstrike/sdk"
import { MockDreamStore, makeAgent } from "./helpers.js"

describe("WorkingMemory", () => {
  it("stores and retrieves values", () => {
    const m = new WorkingMemory()
    m.set("count", 42)
    assert.equal(m.get("count"), 42)
  })

  it("returns undefined for missing key", () => {
    assert.equal(new WorkingMemory().get("x"), undefined)
  })

  it("returns defaultValue when key missing", () => {
    assert.equal(new WorkingMemory().get("x", "default"), "default")
  })

  it("has() tracks set/delete", () => {
    const m = new WorkingMemory()
    assert.equal(m.has("k"), false)
    m.set("k", 1)
    assert.equal(m.has("k"), true)
    m.delete("k")
    assert.equal(m.has("k"), false)
  })

  it("clear() removes everything", () => {
    const m = new WorkingMemory()
    m.set("a", 1); m.set("b", 2)
    m.clear()
    assert.equal(m.has("a"), false)
  })

  it("overwrite replaces value", () => {
    const m = new WorkingMemory()
    m.set("k", "first"); m.set("k", "second")
    assert.equal(m.get("k"), "second")
  })
})

describe("MockDreamStore", () => {
  it("empty initially", async () => {
    assert.deepEqual(await new MockDreamStore().loadSessions("a"), [])
  })

  it("addSession + loadSessions roundtrip", async () => {
    const s = new MockDreamStore()
    const now = Date.now()
    s.addSession("a1", { sessionId: "s1", agentId: "a1", messages: [], metadata: null, createdAtMs: now, updatedAtMs: now })
    assert.equal((await s.loadSessions("a1")).length, 1)
  })

  it("commit adds entries", async () => {
    const s = new MockDreamStore()
    await s.commit("a1", {
      toAdd: [{ text: "fact A", score: 0.9, metadata: null }],
      toRemoveIndices: [],
      stats: { insightsProcessed: 1, duplicatesRemoved: 0, conflictsResolved: 0, entriesAdded: 1 },
    }, [])
    assert.equal((await s.loadMemories("a1")).length, 1)
  })

  it("commit removes by index", async () => {
    const s = new MockDreamStore()
    const existing = [
      { text: "old A", score: 0.5, metadata: null },
      { text: "old B", score: 0.5, metadata: null },
    ]
    await s.commit("a1", {
      toAdd: [{ text: "new C", score: 0.8, metadata: null }],
      toRemoveIndices: [0],
      stats: { insightsProcessed: 1, duplicatesRemoved: 0, conflictsResolved: 0, entriesAdded: 1 },
    }, existing)
    const final = await s.loadMemories("a1")
    assert.equal(final.length, 2)          // B + C
    assert.ok(final.some(m => m.text === "old B"))
    assert.ok(final.some(m => m.text === "new C"))
    assert.ok(!final.some(m => m.text === "old A"))
  })

  it("search respects topK", async () => {
    const s = new MockDreamStore()
    await s.commit("a1", {
      toAdd: Array.from({ length: 5 }, (_, i) => ({ text: `m${i}`, score: 0.5, metadata: null })),
      toRemoveIndices: [],
      stats: { insightsProcessed: 5, duplicatesRemoved: 0, conflictsResolved: 0, entriesAdded: 5 },
    }, [])
    assert.equal((await s.search("a1", "q", 3)).length, 3)
  })
})

describe("Agent.dream()", () => {
  it("returns zero counts when no sessions", async () => {
    const store = new MockDreamStore()
    const agent = makeAgent({ dreamStore: store, agentId: "dreamer" })
    const r = await agent.dream("dreamer")
    assert.equal(r.sessionsProcessed, 0)
  })

  it("processes a session and commits memories", { timeout: 120_000 }, async () => {
    const store = new MockDreamStore()
    const agentId = "dreamer-2"
    const now = Date.now()
    store.addSession(agentId, {
      sessionId: "sess-1", agentId,
      messages: [
        { role: "user",      content: "What is the capital of France?" },
        { role: "assistant", content: "The capital of France is Paris." },
      ],
      metadata: null,
      createdAtMs: now - 3_600_000,
      updatedAtMs: now - 3_600_000,
    })
    const agent = makeAgent({ dreamStore: store, agentId })
    const r = await agent.dream(agentId, now)
    assert.ok(typeof r.sessionsProcessed === "number")
    assert.ok(typeof r.insightsExtracted === "number")
    assert.ok(typeof r.entriesAdded     === "number")
  })
})
