/**
 * `InMemoryDreamStore` — a lightweight `DreamStore` implementation backed by per-agent `Map`s.
 *
 * Originally lived as `MockDreamStore` in the SDK's test helpers; promoted here so benchmarks,
 * examples, and downstream consumers can use it without copying the boilerplate.
 *
 * Use cases:
 *   - Benchmark A/B variants where memory is on/off (preload via constructor).
 *   - Unit tests that exercise `Agent.dream()` or the `memory_query` path without disk I/O.
 *   - Local development / CI where a persistent memory store isn't needed.
 *
 * The `search()` impl is intentionally trivial — it returns the first `topK` memories for the
 * agent regardless of `query`. The kernel ranks by score before deciding what to surface, so the
 * order memories were inserted is what callers see. For semantic search, plug in a real store.
 */
import type {
  CurationResult,
  DreamStore,
  MemoryEntry,
  SessionData,
} from "./protocols.js"

export class InMemoryDreamStore implements DreamStore {
  private sessions = new Map<string, SessionData[]>()
  private memories = new Map<string, MemoryEntry[]>()
  /** Sessions persisted via `saveSession`; exposed for test assertions. */
  readonly savedSessions: SessionData[] = []

  /**
   * @param initialMemories Optional seed memories applied to every agent that asks for memories
   *                        for the first time. Useful for benchmark scenarios that preload a fact.
   */
  constructor(private readonly initialMemories: MemoryEntry[] = []) {}

  /** Pre-populate sessions for a specific agent (test/benchmark setup). */
  addSession(agentId: string, session: SessionData): void {
    const list = this.sessions.get(agentId) ?? []
    list.push(session)
    this.sessions.set(agentId, list)
  }

  /** Pre-populate memories for a specific agent (test/benchmark setup). */
  addMemories(agentId: string, entries: MemoryEntry[]): void {
    this.memories.set(agentId, [...(this.memories.get(agentId) ?? []), ...entries])
  }

  async loadSessions(agentId: string): Promise<SessionData[]> {
    return this.sessions.get(agentId) ?? []
  }

  async loadMemories(agentId: string): Promise<MemoryEntry[]> {
    if (this.memories.has(agentId)) return this.memories.get(agentId)!
    if (this.initialMemories.length > 0) {
      this.memories.set(agentId, [...this.initialMemories])
      return this.memories.get(agentId)!
    }
    return []
  }

  async commit(
    agentId: string,
    result: CurationResult,
    existing: MemoryEntry[],
  ): Promise<void> {
    const kept = existing.filter((_, i) => !result.toRemoveIndices.includes(i))
    this.memories.set(agentId, [...kept, ...result.toAdd])
  }

  async search(agentId: string, _query: string, topK = 5): Promise<MemoryEntry[]> {
    const all = await this.loadMemories(agentId)
    return all.slice(0, topK)
  }

  async saveSession(data: SessionData): Promise<void> {
    this.savedSessions.push(data)
    const list = this.sessions.get(data.agentId) ?? []
    list.push(data)
    this.sessions.set(data.agentId, list)
  }
}
