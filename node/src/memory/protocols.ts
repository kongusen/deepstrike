// ─── Dream / idle-pipeline types ─────────────────────────────────────────────
import type { Message, ContentPart } from "../types.js"

export interface SessionMessage {
  role: Message["role"]
  content: string
  /** Structured multimodal parts. Preserved for round-trip fidelity (e.g. tool result messages). */
  contentParts?: ContentPart[]
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
  /** Persist a completed session for future consolidation via `Agent.dream()`. */
  saveSession(data: SessionData): Promise<void>
}

/** Durable transcript storage for same-session conversational continuity. */

export interface DreamResult {
  sessionsProcessed: number
  insightsExtracted: number
  entriesAdded: number
  entriesRemoved: number
}
