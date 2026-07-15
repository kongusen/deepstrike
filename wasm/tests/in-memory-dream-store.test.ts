import { InMemoryDreamStore } from "../src/memory/in-memory-store.js"
import type { MemoryQuery, MemoryRecord } from "../src/memory/index.js"

const scope = { tenant_id: "tenant-test", namespace: "wasm-store" }
const memory = (content: string, updated_at: number): MemoryRecord => ({
  record_id: `record-${updated_at}`, scope, name: content, kind: "project", content, description: content,
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at, recall_count: 0, confidence: 1, links: [], pinned: false,
})
const query = (text: string): MemoryQuery => ({ scope, query: text, top_k: 5, kinds: [] })

describe("InMemoryDreamStore search", () => {
  it("uses lexical relevance and never returns unrelated fallback entries", async () => {
    const store = new InMemoryDreamStore([
      memory("database migration checklist", 1),
      memory("scheduler fairness in Rust", 2),
      memory("newer unrelated note", 3),
    ])

    await expect(store.search("agent", query("scheduler Rust"))).resolves.toEqual([
      expect.objectContaining({ record: expect.objectContaining({ content: "scheduler fairness in Rust" }) }),
    ])
    await expect(store.search("agent", query("nonexistent"))).resolves.toEqual([])
  })

  // M3-C: score is relevance, not stored confidence (deviation 1).
  it("scores hits by relevance, not stored confidence", async () => {
    const store = new InMemoryDreamStore([
      { ...memory("token rotation and token expiry", 20), record_id: "hi", confidence: 0.1 },
      { ...memory("refresh token expires in UTC", 10), record_id: "lo", confidence: 0.99 },
    ])
    const hits = await store.search("a1", { scope, query: "token expiry rotation", top_k: 2, kinds: [] })
    expect(hits[0]!.record.record_id).toBe("hi")
    expect(hits[0]!.score).toBeGreaterThan(0.1)
    expect(hits[0]!.score).toBeGreaterThan(hits[1]!.score)
  })

  // M3: value-ordered retention eviction + recall/pin mirroring.
  it("evicts the lowest-value record and mirrors recall/pin lifecycle", async () => {
    const store = new InMemoryDreamStore([], { maxRecords: 2 })
    await store.upsert("a1", { ...memory("cold", 1), record_id: "cold", recall_count: 0 })
    await store.upsert("a1", { ...memory("warm", 1), record_id: "warm", recall_count: 5 })
    await store.upsert("a1", { ...memory("new", 1), record_id: "new", recall_count: 1 })
    const ids = (await store.search("a1", { scope, query: "cold warm new", top_k: 9, kinds: [] })).map(h => h.record.record_id)
    expect(ids).not.toContain("cold")

    await store.recordRecall("a1", [{ record_id: "warm", recall_count: 8, last_recalled_at: 3 }])
    const warm = (await store.search("a1", { scope, query: "warm", top_k: 1, kinds: [] }))[0]
    expect(warm?.record.recall_count).toBe(8)
  })
})
