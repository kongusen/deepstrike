// ─── Durable memory and session-extraction types ────────────────────────────
import type { ModelMessage, ContentPart } from "../types.js"

export interface SessionMessage {
  role: ModelMessage["role"]
  content: string
  /** Structured multimodal parts. Preserved for round-trip fidelity (e.g. tool result messages). */
  contentParts?: ContentPart[]
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

export interface MemoryScope {
  tenant_id: string
  namespace: string
}

export interface MemoryProvenance {
  session_id?: string
  author: MemoryAuthor
  trust: MemoryTrustLevel
  evidence_refs: string[]
}

export interface MemoryRecord {
  record_id: string
  scope: MemoryScope
  name: string
  kind: MemoryKind
  content: string
  description: string
  provenance: MemoryProvenance
  created_at: number
  updated_at: number
  last_recalled_at?: number
  recall_count: number
  confidence: number
  links: string[]
  pinned: boolean
  ttl_days?: number
}

export interface MemoryRecall {
  record: MemoryRecord
  score: number
  why: string
}

/** One record's recall lifecycle, mirrored from the kernel's `memory_recalled` observation. */
export interface MemoryRecallLifecycle {
  record_id: string
  recall_count: number
  last_recalled_at: number
}

/** Search options for the public, agent-bound durable `Memory` descriptor. */
export interface MemorySearchOptions {
  topK?: number
  kinds?: MemoryKind[]
  minScore?: number
}

/**
 * Public durable memory descriptor bound to one agent and scope. It intentionally returns records,
 * not ranking internals, and is distinct from `WorkingMemory`, the SDK-only scratch pad.
 */
export interface Memory {
  readonly namespace?: string
  search(query: string, options?: MemorySearchOptions): Promise<MemoryRecord[]>
  get(id: string): Promise<MemoryRecord | null>
  put(record: MemoryRecord): Promise<void>
  delete(id: string): Promise<void>
}

/** Host-owned durable storage behind an agent-bound public `Memory` descriptor. */
export interface MemoryStore {
  put(agentId: string, record: MemoryRecord): Promise<void>
  get(agentId: string, recordId: string): Promise<MemoryRecord | null>
  delete(agentId: string, recordId: string): Promise<void>
  search(agentId: string, query: MemoryQuery): Promise<MemoryRecall[]>
  /** Persist the completed session before the runner performs its one extraction pass. */
  saveSession(data: SessionData): Promise<void>
  /**
   * M3: mirror the kernel's journaled recall lifecycle into the durable store so recall history
   * (count + last-recalled turn) survives across sessions. Optional: a store that does not track
   * recall history omits it. The runner calls it when it observes `memory_recalled`.
   */
  recordRecall?(agentId: string, recalls: MemoryRecallLifecycle[]): Promise<void>
  /**
   * M4: set (or clear) a record's pin. Pinned records are exempt from the store's retention
   * eviction. Optional. The runner calls it when the host/model acts on a promotion suggestion.
   */
  setPinned?(agentId: string, recordId: string, pinned: boolean): Promise<void>
}

/** Optional durable lifecycle capability for lease and memory-write accounting. */
export interface MemoryLifecycleStore {
  issueLease(lease: import("./lifecycle.js").MemoryLease): Promise<import("./lifecycle.js").MemoryLease>
  getLease(agentId: string, scope: MemoryScope): Promise<import("./lifecycle.js").MemoryLease | undefined>
  renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): Promise<import("./lifecycle.js").MemoryLease>
  consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): Promise<import("./lifecycle.js").MemoryLifecycleWriteResult>
  revokeLease(agentId: string, leaseId: string, revokedAt: number): Promise<import("./lifecycle.js").MemoryLease>
  deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): Promise<import("./lifecycle.js").MemoryDeletionReceipt>
  deleted(agentId?: string): Promise<import("./lifecycle.js").MemoryDeletionReceipt[]>
  events(): Promise<import("./lifecycle.js").MemoryLifecycleEvent[]>
}

/** Memory query request (kernel → SDK). */
export interface MemoryQuery {
  scope: MemoryScope
  query: string
  top_k: number
  kinds: MemoryKind[]
  min_score?: number
}

/** Memory validation error (mirroring kernel MemoryValidationError). */
export type MemoryValidationError =
  | { error_kind: "missing_required_field"; field: string }
  | { error_kind: "content_too_large"; size: number; limit: number }
  | { error_kind: "forbidden_pattern"; pattern: string; reason: string }
  | { error_kind: "invalid_kind"; kind: string }
  | { error_kind: "name_too_long"; length: number; limit: number }

/** Durable transcript storage for same-session conversational continuity. */
