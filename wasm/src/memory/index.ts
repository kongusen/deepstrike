export class WorkingMemory {
  private store = new Map<string, unknown>()
  set(key: string, value: unknown): void { this.store.set(key, value) }
  get<T = unknown>(key: string, defaultValue?: T): T | undefined { return (this.store.get(key) as T) ?? defaultValue }
  delete(key: string): void { this.store.delete(key) }
  clear(): void { this.store.clear() }
  has(key: string): boolean { return this.store.has(key) }
}

export interface SessionMessage {
  role: "user" | "assistant" | "tool"
  content: string
  tokenCount?: number
  toolCalls?: Array<{ id: string; name: string; arguments: string }>
}

export interface SessionData {
  sessionId: string
  agentId: string
  messages: SessionMessage[]
  metadata: unknown
  createdAtMs: number
  updatedAtMs: number
}

export interface MemoryEntry {
  text: string
  score: number
  metadata: unknown
}

export interface CurationStats {
  insightsProcessed: number
  duplicatesRemoved: number
  conflictsResolved: number
  entriesAdded: number
}

export interface CurationResult {
  toAdd: MemoryEntry[]
  toRemoveIndices: number[]
  stats: CurationStats
}

export interface DreamResult {
  sessionsProcessed: number
  insightsExtracted: number
  entriesAdded: number
  entriesRemoved: number
}

export interface DreamStore {
  loadSessions(agentId: string): Promise<SessionData[]>
  loadMemories(agentId: string): Promise<MemoryEntry[]>
  commit(agentId: string, result: CurationResult, existing: MemoryEntry[]): Promise<void>
  search(agentId: string, query: string, topK?: number): Promise<MemoryEntry[]>
  /** Persist a completed session for future consolidation via `Agent.dream()`. */
  saveSession(data: SessionData): Promise<void>
}
