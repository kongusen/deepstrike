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

// ─── Phase 7: Long-term memory types (mirroring kernel mm/memory.rs) ─────────

/** Memory kind (4 types, mirroring Claude Code). */
export type MemoryKind = "user" | "feedback" | "project" | "reference"

/** Memory metadata (kernel stores, SDK provides full content). */
export interface MemoryMetadata {
  name: string
  description: string
  kind?: MemoryKind
  created_at: number
  updated_at: number
  session_id?: string

  // Heuristic inference fields
  user_role?: string
  expertise_level?: string
  preference_rule?: string
  approved_pattern?: string
  project_phase?: string
  relative_date?: string
  external_url?: string
  ticket_ref?: string
}

/** Memory write request (SDK → kernel). */
export interface MemoryWriteRequest {
  metadata: MemoryMetadata
  content: string
}

/** Memory query request (kernel → SDK). */
export interface MemoryQuery {
  current_context: string
  active_tools: string[]
  already_surfaced: string[]
  top_k: number
}

/** Memory retrieval response (SDK → kernel). */
export interface MemoryRetrieval {
  selected_memory_ids: string[]
  selection_rationale: string
}

/** Memory validation error (mirroring kernel MemoryValidationError). */
export type MemoryValidationError =
  | { kind: "missing_required_field"; field: string }
  | { kind: "content_too_large"; size: number; limit: number }
  | { kind: "forbidden_pattern"; pattern: string; reason: string }
  | { kind: "invalid_kind"; kind: string }
  | { kind: "name_too_long"; length: number; limit: number }

/** Durable transcript storage for same-session conversational continuity. */

export interface DreamResult {
  sessionsProcessed: number
  insightsExtracted: number
  entriesAdded: number
  entriesRemoved: number
}
