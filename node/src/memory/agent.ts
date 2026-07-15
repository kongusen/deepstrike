/**
 * Long-term memory agent implementation (Phase 7).
 *
 * Provides deterministic selection and boundary validation for long-term memories.
 *
 * Design principles:
 * - Kernel defines memory types and validation rules
 * - SDK performs I/O and selection
 * - Semantic reranking remains an optional DreamStore capability
 */

import type {
  MemoryKind,
  MemoryQuery,
  MemoryRecall,
  MemoryRecord,
} from "./protocols.js"
import { rankMemories } from "./ranking.js"

// Long-term memory types live in `protocols.ts` as the single source of truth (mirroring kernel
// `mm/memory.rs`). Re-exported here so existing `from "./memory/agent.js"` imports keep working.
export type {
  MemoryKind,
  MemoryQuery,
  MemoryRecall,
  MemoryRecord,
} from "./protocols.js"

/**
 * Select relevant memories for the current context.
 *
 * The reference selector is deterministic and provider-independent: lexical overlap first,
 * recency second. Semantic/embedding reranking belongs in a DreamStore plugin.
 *
 * @param query - Memory query from kernel
 * @param memoryIndex - Memory index entries
 * @param model - Optional model name (default: claude-sonnet-4-20250514)
 * @returns Memory retrieval result
 */
export async function selectMemories(
  query: MemoryQuery,
  records: MemoryRecord[],
): Promise<MemoryRecall[]> {
  const candidates = records.filter(record =>
    record.scope.tenant_id === query.scope.tenant_id
    && record.scope.namespace === query.scope.namespace
    && (query.kinds.length === 0 || query.kinds.includes(record.kind))
    && (query.min_score === undefined || record.confidence >= query.min_score),
  )
  const selected = rankMemories(query.query, candidates.map((record, insertionIndex) => ({
    value: record,
    searchableText: `${record.name} ${record.description} ${record.content}`,
    updatedAt: Number.isFinite(record.updated_at) ? record.updated_at : 0,
    insertionIndex,
  })), query.top_k)
  return selected.map(record => ({
    record,
    score: Math.max(0, Math.min(1, record.confidence)),
    why: "deterministic lexical relevance with recency tie-breaking",
  }))
}

/**
 * Validate memory before writing (kernel-side validation mirror).
 *
 * This SDK-side validation provides early feedback before sending to kernel.
 */
export function validateMemory(record: MemoryRecord): { valid: boolean; error?: string } {
  // Check required fields
  if (!record.record_id || record.record_id.trim().length === 0) {
    return { valid: false, error: "Missing required field: record_id" }
  }
  if (!record.scope.tenant_id || !record.scope.namespace) {
    return { valid: false, error: "Missing required field: scope" }
  }
  if (!record.name || record.name.trim().length === 0) {
    return { valid: false, error: "Missing required field: name" }
  }
  if (!record.description || record.description.trim().length === 0) {
    return { valid: false, error: "Missing required field: description" }
  }

  // Check name length
  if (record.name.length > 100) {
    return { valid: false, error: `Name too long: ${record.name.length} chars (limit: 100)` }
  }

  // Check content size
  if (record.content.length > 10_000) {
    return { valid: false, error: `Content too large: ${record.content.length} bytes (limit: 10000)` }
  }

  return { valid: true }
}
