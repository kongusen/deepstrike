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
})
