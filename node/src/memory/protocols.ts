// ─── Dream / idle-pipeline types ─────────────────────────────────────────────

export interface SessionMessage {
  role: string
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
  /** Indices into the `existingMemories` array passed to `DreamStore.loadMemories`. */
  toRemoveIndices: number[]
  stats: CurationStats
}

export interface DreamStore {
  loadSessions(agentId: string): Promise<SessionData[]>
  loadMemories(agentId: string): Promise<MemoryEntry[]>
  commit(agentId: string, result: CurationResult, existing: MemoryEntry[]): Promise<void>
  /** Semantic search over the agent's long-term memories. Called on demand during a run. */
  search(agentId: string, query: string, topK?: number): Promise<MemoryEntry[]>
}

export interface DreamResult {
  sessionsProcessed: number
  insightsExtracted: number
  entriesAdded: number
  entriesRemoved: number
}
