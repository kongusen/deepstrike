import { InMemoryDreamStore } from "../src/memory/in-memory-store.js"
import type { MemoryQuery, MemoryRecord, SessionData } from "../src/memory/protocols.js"

const scope = { tenant_id: "tenant-test", namespace: "store-tests" }
const memory = (content: string, confidence = 0.9, updated_at = 1): MemoryRecord => ({
  record_id: `record-${content}`, scope, name: content, kind: "project", content,
  description: content,
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 1, updated_at, recall_count: 0, confidence, links: [], pinned: false,
})
const query = (text: string, top_k: number): MemoryQuery => ({ scope, query: text, top_k, kinds: [] })

const session = (agentId: string, sessionId = "s1"): SessionData => ({
  sessionId,
  agentId,
  messages: [],
  metadata: null,
  createdAtMs: 1,
  updatedAtMs: 1,
})

describe("InMemoryDreamStore", () => {
  it("returns no search results for an unknown agent", async () => {
    const store = new InMemoryDreamStore()
    expect(await store.search("agent-x", query("anything", 5))).toEqual([])
  })

  it("seeds initial memories independently per agent", async () => {
    const seed = [memory("fact-a")]
    const store = new InMemoryDreamStore(seed)
    expect((await store.search("a1", query("fact", 5))).map(hit => hit.record.content)).toEqual(["fact-a"])
    expect((await store.search("a2", query("fact", 5))).map(hit => hit.record.content)).toEqual(["fact-a"])
  })

  it("upserts by scoped kind and name", async () => {
    const store = new InMemoryDreamStore()
    await store.upsert("a1", memory("a", 0.5))
    await store.upsert("a1", { ...memory("a", 0.9), content: "updated" })
    expect((await store.search("a1", query("updated", 5))).map(hit => hit.record.content)).toEqual(["updated"])
  })

  it("search ranks lexical matches instead of returning insertion order", async () => {
    const store = new InMemoryDreamStore([
      memory("database migration notes", 0.9, 30),
      memory("refresh token expires in UTC", 0.2, 10),
      memory("token rotation and token expiry", 0.1, 20),
    ])
    expect((await store.search("a1", query("token expiry", 2))).map(hit => hit.record.content)).toEqual([
      "token rotation and token expiry",
      "refresh token expires in UTC",
    ])
  })

  it("uses recency and then insertion order as deterministic tie-breakers", async () => {
    const store = new InMemoryDreamStore([
      memory("auth token alpha", 0.1, 10),
      memory("auth token beta", 0.9, 20),
      memory("auth token gamma", 0.5, 20),
    ])
    expect((await store.search("a1", query("auth token", 3))).map(hit => hit.record.content)).toEqual([
      "auth token beta",
      "auth token gamma",
      "auth token alpha",
    ])
  })

  it("returns no memories when a non-empty query has no lexical match", async () => {
    const store = new InMemoryDreamStore([
      memory("database migration notes", 0.9, 30),
    ])
    expect(await store.search("a1", query("oauth token", 5))).toEqual([])
  })

  it("matches CJK query phrases without requiring whitespace tokenization", async () => {
    const store = new InMemoryDreamStore([
      memory("数据库迁移注意事项", 0.9, 30),
      memory("刷新令牌过期时间使用 UTC", 0.2, 10),
    ])
    expect((await store.search("a1", query("令牌过期", 1))).map(hit => hit.record.content)).toEqual([
      "刷新令牌过期时间使用 UTC",
    ])
  })

  it("saveSession persists the completed transcript", async () => {
    const store = new InMemoryDreamStore()
    await store.saveSession(session("a1", "s-saved"))
    expect(store.savedSessions.length).toBe(1)
    expect(store.savedSessions[0]?.sessionId).toBe("s-saved")
  })

  // M3-C: the recall score is relevance, not the record's stored confidence (deviation 1).
  it("scores hits by relevance, not by stored confidence", async () => {
    const store = new InMemoryDreamStore([
      memory("token rotation and token expiry", 0.1, 20), // low confidence, high lexical overlap
      memory("refresh token expires in UTC", 0.99, 10),   // high confidence, lower overlap
    ])
    const hits = await store.search("a1", query("token expiry rotation", 2))
    expect(hits[0]?.record.content).toBe("token rotation and token expiry")
    // Score is a relevance figure in (0,1], unrelated to the 0.1 confidence it was stored with.
    expect(hits[0]!.score).toBeGreaterThan(0.1)
    expect(hits[0]!.score).toBeGreaterThan(hits[1]!.score)
    expect(hits[0]!.why).toContain("lexical")
  })

  // M3-C: TTL/staleness discounts an old record at recall time (host owns the clock).
  it("discounts a stale record's relevance below a fresh one with equal overlap", async () => {
    const now = 100 * 86_400_000 // day 100
    const fresh = { ...memory("deploy runbook steps", 0.5, now), record_id: "fresh" }
    const stale = { ...memory("deploy runbook steps", 0.5, 0), record_id: "stale", ttl_days: 5 }
    const store = new InMemoryDreamStore([fresh, stale], { now: () => now, staleWarningDays: 2 })
    const hits = await store.search("a1", query("deploy runbook", 2))
    expect(hits.map(h => h.record.record_id)).toEqual(["fresh", "stale"])
    expect(hits[0]!.score).toBeGreaterThan(hits[1]!.score)
  })

  // M3: value-ordered retention eviction replaces the deleted blind tail-cut.
  it("evicts the lowest-value unpinned record past maxRecords", async () => {
    const store = new InMemoryDreamStore([], { maxRecords: 2 })
    await store.upsert("a1", { ...memory("cold"), record_id: "cold", recall_count: 0 })
    await store.upsert("a1", { ...memory("warm"), record_id: "warm", recall_count: 5 })
    await store.upsert("a1", { ...memory("new"), record_id: "new", recall_count: 1 })
    const ids = (await store.search("a1", query("cold warm new", 9))).map(h => h.record.record_id)
    expect(ids).not.toContain("cold") // never recalled → lowest value → evicted
    expect(ids).toEqual(expect.arrayContaining(["warm", "new"]))
  })

  it("never evicts a pinned record even when it is otherwise lowest value", async () => {
    const store = new InMemoryDreamStore([], { maxRecords: 1 })
    await store.upsert("a1", { ...memory("pinned"), record_id: "pinned", recall_count: 0, pinned: true })
    await store.upsert("a1", { ...memory("hot"), record_id: "hot", recall_count: 9 })
    const ids = (await store.search("a1", query("pinned hot", 9))).map(h => h.record.record_id)
    expect(ids).toContain("pinned")
    expect(ids).not.toContain("hot")
  })

  // M3: recall journaling mirrored from the kernel observation.
  it("recordRecall mirrors count and last-recalled into the durable record", async () => {
    const store = new InMemoryDreamStore([memory("a-fact")])
    await store.recordRecall("a1", [{ record_id: "record-a-fact", recall_count: 3, last_recalled_at: 42 }])
    const hit = (await store.search("a1", query("a-fact", 1)))[0]
    expect(hit?.record.recall_count).toBe(3)
    expect(hit?.record.last_recalled_at).toBe(42)
  })

  // M4: pin mirrored from the host acting on a promotion suggestion.
  it("setPinned marks a record exempt from eviction", async () => {
    const store = new InMemoryDreamStore([{ ...memory("keep"), record_id: "keep", recall_count: 0 }], { maxRecords: 1 })
    await store.setPinned("a1", "keep", true)
    await store.upsert("a1", { ...memory("hot"), record_id: "hot", recall_count: 9 })
    const ids = (await store.search("a1", query("keep hot", 9))).map(h => h.record.record_id)
    expect(ids).toContain("keep")
  })
})
