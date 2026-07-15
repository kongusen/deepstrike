/**
 * `InMemoryDreamStore` — a lightweight `DreamStore` implementation backed by per-agent `Map`s.
 *
 * Originally lived as `MockDreamStore` in the SDK's test helpers; promoted here so benchmarks,
 * examples, and downstream consumers can use it without copying the boilerplate.
 *
 * Use cases:
 *   - Benchmark A/B variants where memory is on/off (preload via constructor).
 *   - Unit tests that exercise session extraction or the `memory_query` path without disk I/O.
 *   - Local development / CI where a persistent memory store isn't needed.
 *
 * Search returns a genuine relevance score (distinct lexical overlap, lifted by recall history and
 * lowered by TTL/staleness) — never the record's stored confidence. The store is the authority for
 * the full record set, so it (M3) bounds itself by value-ordered retention eviction and (M3/M4)
 * mirrors recall lifecycle and pin state. For semantic search, plug in a real store.
 */
import type {
  DreamStore,
  MemoryQuery,
  MemoryRecall,
  MemoryRecallLifecycle,
  MemoryRecord,
  SessionData,
} from "./protocols.js"
import { rankMemories } from "./ranking.js"
import { memoryRetentionScore } from "./retention.js"

export interface InMemoryDreamStoreOptions {
  /** Cap the per-agent record set; a write past it evicts the lowest-value unpinned records (M3). */
  maxRecords?: number
  /** Age (days) past which a record's recall relevance is discounted. Default 2. */
  staleWarningDays?: number
  /** Wall-clock source for staleness scoring + recall stamps. Injectable for deterministic tests. */
  now?: () => number
}

export class InMemoryDreamStore implements DreamStore {
  private memories = new Map<string, MemoryRecord[]>()
  /** Sessions persisted via `saveSession`; exposed for test assertions. */
  readonly savedSessions: SessionData[] = []
  private readonly maxRecords?: number
  private readonly staleWarningDays: number
  private readonly now: () => number

  /**
   * @param initialMemories Optional seed memories applied to every agent that asks for memories
   *                        for the first time. Useful for benchmark scenarios that preload a fact.
   */
  constructor(
    private readonly initialMemories: MemoryRecord[] = [],
    options: InMemoryDreamStoreOptions = {},
  ) {
    this.maxRecords = options.maxRecords
    this.staleWarningDays = options.staleWarningDays ?? 2
    this.now = options.now ?? Date.now
  }

  private recordsFor(agentId: string): MemoryRecord[] {
    if (this.memories.has(agentId)) return this.memories.get(agentId)!
    if (this.initialMemories.length > 0) {
      this.memories.set(agentId, [...this.initialMemories])
      return this.memories.get(agentId)!
    }
    return []
  }

  async upsert(agentId: string, incoming: MemoryRecord): Promise<void> {
    const kept = [...this.recordsFor(agentId)]
    const index = kept.findIndex(record =>
        record.scope.tenant_id === incoming.scope.tenant_id
        && record.scope.namespace === incoming.scope.namespace
        && record.kind === incoming.kind
        && record.name === incoming.name,
    )
    if (index >= 0) kept[index] = incoming
    else kept.push(incoming)
    this.memories.set(agentId, this.evictToCapacity(kept))
  }

  /** M3: value-ordered retention eviction. Sheds the lowest-value unpinned records — the shared
   *  deterministic formula, never a blind tail-cut — until the set fits `maxRecords`. */
  private evictToCapacity(records: MemoryRecord[]): MemoryRecord[] {
    if (this.maxRecords === undefined || records.length <= this.maxRecords) return records
    const nowMs = this.now()
    const scored = records.map((record, insertionIndex) => ({
      record,
      insertionIndex,
      score: record.pinned ? Number.POSITIVE_INFINITY : memoryRetentionScore(record, nowMs, this.staleWarningDays),
    }))
    // Keep the highest-value maxRecords; ties break on insertion order (older first survives).
    scored.sort((a, b) => b.score - a.score || a.insertionIndex - b.insertionIndex)
    return scored.slice(0, this.maxRecords).map(entry => entry.record)
  }

  async search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]> {
    const all = this.recordsFor(agentId)
    const candidates = all.filter(record =>
      record.scope.tenant_id === query.scope.tenant_id
      && record.scope.namespace === query.scope.namespace
      && (query.kinds.length === 0 || query.kinds.includes(record.kind)),
    )
    const ranked = rankMemories(query.query, candidates.map((record, insertionIndex) => {
      return {
        value: record,
        searchableText: `${record.name} ${record.description} ${record.content}`,
        updatedAt: Number.isFinite(record.updated_at) ? record.updated_at : 0,
        recallCount: record.recall_count,
        ttlDays: record.ttl_days,
        insertionIndex,
      }
    }), query.top_k, { nowMs: this.now(), staleWarningDays: this.staleWarningDays })
    return ranked
      .filter(hit => query.min_score === undefined || hit.score >= query.min_score)
      .map(hit => ({ record: hit.value, score: hit.score, why: hit.why }))
  }

  async saveSession(data: SessionData): Promise<void> {
    this.savedSessions.push(data)
  }

  /** M3: mirror the kernel's journaled recall lifecycle into the durable records. */
  async recordRecall(agentId: string, recalls: MemoryRecallLifecycle[]): Promise<void> {
    const records = this.recordsFor(agentId)
    for (const recall of recalls) {
      const record = records.find(candidate => candidate.record_id === recall.record_id)
      if (record) {
        record.recall_count = recall.recall_count
        record.last_recalled_at = recall.last_recalled_at
      }
    }
    this.memories.set(agentId, records)
  }

  /** M4: set a record's pin so retention eviction cannot shed it. */
  async setPinned(agentId: string, recordId: string, pinned: boolean): Promise<void> {
    const records = this.recordsFor(agentId)
    const record = records.find(candidate => candidate.record_id === recordId)
    if (record) record.pinned = pinned
    this.memories.set(agentId, records)
  }
}
