export class WorkingMemory {
  private store = new Map<string, unknown>()
  set(key: string, value: unknown): void { this.store.set(key, value) }
  get<T = unknown>(key: string, defaultValue?: T): T | undefined { return (this.store.get(key) as T) ?? defaultValue }
  delete(key: string): void { this.store.delete(key) }
  clear(): void { this.store.clear() }
  has(key: string): boolean { return this.store.has(key) }
}

export interface SessionMessage {
  role: "system" | "user" | "assistant" | "tool"
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

export type MemoryKind = "user" | "feedback" | "project" | "reference"
export type MemoryAuthor = "model" | "host" | "extraction"
export type MemoryTrustLevel = "untrusted" | "user_asserted" | "host_verified"
export interface MemoryScope { tenant_id: string; namespace: string }
export interface MemoryProvenance {
  session_id?: string
  author: MemoryAuthor
  trust: MemoryTrustLevel
  evidence_refs: string[]
}
export interface MemoryRecord {
  record_id: string; scope: MemoryScope; name: string; kind: MemoryKind; content: string
  description: string; provenance: MemoryProvenance; created_at: number; updated_at: number
  last_recalled_at?: number; recall_count: number; confidence: number; links: string[]
  pinned: boolean; ttl_days?: number
}
export interface MemoryRecall { record: MemoryRecord; score: number; why: string }
export interface MemoryQuery {
  scope: MemoryScope; query: string; top_k: number; kinds: MemoryKind[]; min_score?: number
}
/** One record's recall lifecycle, mirrored from the kernel's `memory_recalled` observation (M3). */
export interface MemoryRecallLifecycle { record_id: string; recall_count: number; last_recalled_at: number }

export interface DreamStore {
  upsert(agentId: string, record: MemoryRecord): Promise<void>
  search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]>
  /** Persist a completed session before the runner's one extraction pass. */
  saveSession(data: SessionData): Promise<void>
  /** M3: mirror the kernel's journaled recall lifecycle into the durable store. Optional. */
  recordRecall?(agentId: string, recalls: MemoryRecallLifecycle[]): Promise<void>
  /** M4: set/clear a record's pin (exempt from retention eviction). Optional. */
  setPinned?(agentId: string, recordId: string, pinned: boolean): Promise<void>
}

export interface SessionStore {
  loadSession(sessionId: string): Promise<SessionData | undefined>
  saveSession(data: SessionData): Promise<void>
}
