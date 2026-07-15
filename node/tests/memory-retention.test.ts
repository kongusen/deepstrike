import { memoryRetentionScore } from "../src/memory/retention.js"
import type { MemoryRecord } from "../src/memory/protocols.js"

const base = (over: Partial<MemoryRecord> = {}): MemoryRecord => ({
  record_id: "r", scope: { tenant_id: "t", namespace: "n" }, name: "n", kind: "reference",
  content: "x".repeat(100), description: "d",
  provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
  created_at: 0, updated_at: 0, recall_count: 0, confidence: 0, links: [], pinned: false,
  ...over,
})

describe("memoryRetentionScore (host mirror of the kernel value vocabulary)", () => {
  it("matches the kernel reference for the terms both compute", () => {
    // Reference from value.rs: reference kind 1200, tokens=100/4=25 → size 100, usage/recency/
    // confidence/staleness all 0. Score = 1200 - 100 = 1100.
    expect(memoryRetentionScore(base(), 0, 2)).toBe(1100)
    // recall_count 3 → usageBucket floor(log2(4))=2 → usage 2*8192=16384. 16384+1200-100 = 17484.
    expect(memoryRetentionScore(base({ recall_count: 3 }), 0, 2)).toBe(17484)
  })

  it("a recalled record beats a cold one; a pin is absolute", () => {
    expect(memoryRetentionScore(base({ recall_count: 3 }), 0, 2))
      .toBeGreaterThan(memoryRetentionScore(base(), 0, 2))
    expect(memoryRetentionScore(base({ pinned: true }), 0, 2)).toBe(Number.POSITIVE_INFINITY)
  })

  it("TTL/staleness discount lowers the score for an aged record", () => {
    const now = 100 * 86_400_000
    const fresh = memoryRetentionScore(base({ updated_at: now }), now, 2)
    const stale = memoryRetentionScore(base({ updated_at: 0, ttl_days: 5 }), now, 2)
    expect(stale).toBeLessThan(fresh)
  })
})
