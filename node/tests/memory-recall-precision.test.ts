/**
 * P3 gate for the memory M3 work: the relevance ranker must beat a FIFO baseline on precision@k
 * over a planted corpus. This is the "召回质量探针" acceptance criterion from the six-mechanisms
 * spec §6.3 — a regression here means recall quality silently fell back toward insertion order.
 */
import { InMemoryDreamStore } from "../src/memory/in-memory-store.js"
import type { MemoryQuery, MemoryRecord } from "../src/memory/protocols.js"

const scope = { tenant_id: "t", namespace: "precision" }

function record(id: string, text: string, updatedAt: number): MemoryRecord {
  return {
    record_id: id, scope, name: id, kind: "project", content: text, description: text,
    provenance: { author: "extraction", trust: "untrusted", evidence_refs: [] },
    created_at: 1, updated_at: updatedAt, recall_count: 0, confidence: 0.5, links: [], pinned: false,
  }
}

const q = (text: string, top_k: number): MemoryQuery => ({ scope, query: text, top_k, kinds: [] })

describe("memory recall precision@k (P3 gate)", () => {
  // A corpus where the newest records are decoys and the relevant records are older — a FIFO/
  // recency ranker would surface the decoys; a relevance ranker must find the planted matches.
  const corpus: MemoryRecord[] = [
    record("rel-1", "kubernetes pod eviction policy and resource limits", 1),
    record("rel-2", "pod eviction under memory pressure in kubernetes", 2),
    record("decoy-1", "quarterly finance report spreadsheet", 100),
    record("decoy-2", "team offsite lunch schedule", 101),
    record("decoy-3", "git rebase interactive workflow notes", 102),
  ]
  const relevant = new Set(["rel-1", "rel-2"])

  it("relevance ranking achieves precision@2 = 1.0 where FIFO/recency would score 0", async () => {
    const store = new InMemoryDreamStore(corpus)
    const hits = await store.search("a", q("kubernetes pod eviction", 2))
    const precision = hits.filter(h => relevant.has(h.record.record_id)).length / hits.length
    expect(precision).toBe(1)

    // Baseline: take the newest-2 by updated_at (what the pre-M3 FIFO-ish path surfaced). Its
    // precision is 0 on this corpus — the gate's whole point is that relevance beats it.
    const fifoTop2 = [...corpus].sort((a, b) => b.updated_at - a.updated_at).slice(0, 2)
    const fifoPrecision = fifoTop2.filter(r => relevant.has(r.record_id)).length / 2
    expect(fifoPrecision).toBe(0)
    expect(precision).toBeGreaterThan(fifoPrecision)
  })

  it("a non-matching query returns nothing rather than recency-nearest decoys", async () => {
    const store = new InMemoryDreamStore(corpus)
    expect(await store.search("a", q("unrelated astronomy telescope", 3))).toEqual([])
  })
})
