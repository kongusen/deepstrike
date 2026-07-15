/**
 * `InMemoryDreamStore` — a lightweight `DreamStore` implementation backed by per-agent `Map`s.
 * WASM port of node/src/memory/in-memory-store.ts. See that file for the full design.
 *
 * Search returns a genuine relevance score (never stored confidence); the store bounds itself by
 * value-ordered retention eviction (M3) and mirrors recall lifecycle and pin state (M3/M4).
 */
import type {
  DreamStore, MemoryQuery, MemoryRecall, MemoryRecallLifecycle, MemoryRecord, SessionData,
} from "./index.js"
import { rankMemories } from "./ranking.js"
import { memoryRetentionScore } from "./retention.js"

export interface InMemoryDreamStoreOptions {
  maxRecords?: number
  staleWarningDays?: number
  now?: () => number
}

export class InMemoryDreamStore implements DreamStore {
  private memories = new Map<string, MemoryRecord[]>()
  readonly savedSessions: SessionData[] = []
  private readonly maxRecords?: number
  private readonly staleWarningDays: number
  private readonly now: () => number

  constructor(private readonly initialMemories: MemoryRecord[] = [], options: InMemoryDreamStoreOptions = {}) {
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

  private evictToCapacity(records: MemoryRecord[]): MemoryRecord[] {
    if (this.maxRecords === undefined || records.length <= this.maxRecords) return records
    const nowMs = this.now()
    const scored = records.map((record, insertionIndex) => ({
      record,
      insertionIndex,
      score: record.pinned ? Number.POSITIVE_INFINITY : memoryRetentionScore(record, nowMs, this.staleWarningDays),
    }))
    scored.sort((a, b) => b.score - a.score || a.insertionIndex - b.insertionIndex)
    return scored.slice(0, this.maxRecords).map(entry => entry.record)
  }

  async upsert(agentId: string, incoming: MemoryRecord): Promise<void> {
    const kept = [...this.recordsFor(agentId)]
    const index = kept.findIndex(record => record.scope.tenant_id === incoming.scope.tenant_id
        && record.scope.namespace === incoming.scope.namespace
        && record.kind === incoming.kind && record.name === incoming.name)
    if (index >= 0) kept[index] = incoming
    else kept.push(incoming)
    this.memories.set(agentId, this.evictToCapacity(kept))
  }

  async search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]> {
    const all = this.recordsFor(agentId)
    const candidates = all.filter(record => record.scope.tenant_id === query.scope.tenant_id
      && record.scope.namespace === query.scope.namespace
      && (query.kinds.length === 0 || query.kinds.includes(record.kind)))
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

  async setPinned(agentId: string, recordId: string, pinned: boolean): Promise<void> {
    const records = this.recordsFor(agentId)
    const record = records.find(candidate => candidate.record_id === recordId)
    if (record) record.pinned = pinned
    this.memories.set(agentId, records)
  }
}
