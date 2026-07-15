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
 * Search is a deterministic local reference implementation: distinct lexical overlap first,
 * `metadata.updated_at` recency second, and insertion order as the final stable tie-breaker.
 * Non-empty queries never return unrelated entries. For semantic search, plug in a real store.
 */
import type {
  DreamStore,
  MemoryQuery,
  MemoryRecall,
  MemoryRecord,
  SessionData,
} from "./protocols.js"
import { rankMemories } from "./ranking.js"

export class InMemoryDreamStore implements DreamStore {
  private memories = new Map<string, MemoryRecord[]>()
  /** Sessions persisted via `saveSession`; exposed for test assertions. */
  readonly savedSessions: SessionData[] = []

  /**
   * @param initialMemories Optional seed memories applied to every agent that asks for memories
   *                        for the first time. Useful for benchmark scenarios that preload a fact.
   */
  constructor(private readonly initialMemories: MemoryRecord[] = []) {}

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
    this.memories.set(agentId, kept)
  }

  async search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]> {
    const all = this.recordsFor(agentId)
    const candidates = all.filter(record =>
      record.scope.tenant_id === query.scope.tenant_id
      && record.scope.namespace === query.scope.namespace
      && (query.kinds.length === 0 || query.kinds.includes(record.kind))
      && (query.min_score === undefined || record.confidence >= query.min_score),
    )
    const selected = rankMemories(query.query, candidates.map((record, insertionIndex) => {
      return {
        value: record,
        searchableText: `${record.name} ${record.description} ${record.content}`,
        updatedAt: Number.isFinite(record.updated_at) ? record.updated_at : 0,
        insertionIndex,
      }
    }), query.top_k)
    return selected.map(record => ({
      record,
      score: Math.max(0, Math.min(1, record.confidence)),
      why: "deterministic lexical relevance with recency tie-breaking",
    }))
  }

  async saveSession(data: SessionData): Promise<void> {
    this.savedSessions.push(data)
  }
}
