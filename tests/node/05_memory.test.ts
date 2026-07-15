/**
 * 05_memory.test.ts — WorkingMemory + DreamStore
 */
import { describe, it } from "node:test"
import assert from "node:assert/strict"
import { WorkingMemory } from "@deepstrike/sdk"
import { MockDreamStore, TEST_MEMORY_SCOPE, memoryRecord } from "./helpers.js"

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
  it("upsert adds entries", async () => {
    const s = new MockDreamStore()
    await s.upsert("a1", memoryRecord("fact-a", "fact A", 0.9))
    assert.equal((await s.search("a1", { scope: TEST_MEMORY_SCOPE, query: "fact", top_k: 5, kinds: [] })).length, 1)
  })

  it("search respects topK", async () => {
    const s = new MockDreamStore()
    for (let i = 0; i < 5; i++) await s.upsert("a1", memoryRecord(`m-${i}`, `m${i}`))
    assert.equal((await s.search("a1", {
      scope: TEST_MEMORY_SCOPE,
      query: "q",
      top_k: 3,
      kinds: [],
    })).length, 3)
  })
})
