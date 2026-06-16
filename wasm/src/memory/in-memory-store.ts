/**
 * `InMemoryDreamStore` — a lightweight `DreamStore` implementation backed by per-agent `Map`s.
 * WASM port of node/src/memory/in-memory-store.ts. See that file for the full design.
 */
import type { CurationResult, DreamStore, MemoryEntry, SessionData } from "./index.js"

export class InMemoryDreamStore implements DreamStore {
  private sessions = new Map<string, SessionData[]>()
  private memories = new Map<string, MemoryEntry[]>()
  readonly savedSessions: SessionData[] = []

  constructor(private readonly initialMemories: MemoryEntry[] = []) {}

  addSession(agentId: string, session: SessionData): void {
    const list = this.sessions.get(agentId) ?? []
    list.push(session)
    this.sessions.set(agentId, list)
  }

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
