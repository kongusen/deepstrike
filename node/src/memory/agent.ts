/**
 * Long-term memory agent implementation (Phase 7).
 *
 * Provides deterministic selection and boundary validation for long-term memories.
 *
 * Design principles:
 * - Kernel defines memory types and validation rules
 * - SDK performs I/O and selection
 * - Semantic reranking remains an optional MemoryStore capability
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
 * recency second. Semantic/embedding reranking belongs in a MemoryStore plugin.
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
    && (query.kinds.length === 0 || query.kinds.includes(record.kind)),
  )
  const ranked = rankMemories(query.query, candidates.map((record, insertionIndex) => ({
    value: record,
    searchableText: `${record.name} ${record.description} ${record.content}`,
    updatedAt: Number.isFinite(record.updated_at) ? record.updated_at : 0,
    recallCount: record.recall_count,
    ttlDays: record.ttl_days,
    insertionIndex,
  })), query.top_k)
  // score is relevance (from ranking), deliberately distinct from the record's stored confidence.
  return ranked
    .filter(hit => query.min_score === undefined || hit.score >= query.min_score)
    .map(hit => ({ record: hit.value, score: hit.score, why: hit.why }))
}

/**
 * Check a record's shape before it is offered for writing: identity, scope and description.
 *
 * The admission rule itself — name and content limits, and whether validation is on at all — is
 * the kernel's alone (`admit_memory_write` in a run, the stateless memory authority outside one).
 * This function deliberately carries no copy of it.
 */
export function validateMemory(record: MemoryRecord): { valid: boolean; error?: string } {
  if (!record.record_id || record.record_id.trim().length === 0) {
    return { valid: false, error: "Missing required field: record_id" }
  }
  if (!record.scope.tenant_id || !record.scope.namespace) {
    return { valid: false, error: "Missing required field: scope" }
  }
  if (!record.description || record.description.trim().length === 0) {
    return { valid: false, error: "Missing required field: description" }
  }
  return { valid: true }
}
