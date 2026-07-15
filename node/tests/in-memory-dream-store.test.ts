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
})
