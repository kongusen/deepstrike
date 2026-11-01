// M5 · how a recalled record enters the model's context. Recall pushes an index line, not the
// record: the header names the record, and the body is its one-line description (its name when it
// has none). The model reads a record in full with `memory(record_id)` when it judges it relevant,
// so a prefetch costs a line per hit, not a record per hit. Every prefetch — the turn-1 one and each
// renewal re-query — lands its hits as keyed knowledge `memory:<record_id>`: a running operation
// cannot append to its own history, and one placement lets the key dedupe a record across phases.
// One rule in every SDK, pinned by the same vectors in each SDK's test.

import { memoryRecordExpired, memoryScopeMatches } from "./lifecycle.js"
import type { MemoryQuery, MemoryRecall, MemoryRecord } from "./protocols.js"

/** The index line one recalled record becomes. The header names the record and its trust level
 *  so the model can tell host-verified facts from untrusted ones; the retrieval score is left out
 *  because it changes between retrievals while the record does not. */
export function memoryRecallTurn(record: MemoryRecord): string {
  const summary = record.description.trim() || record.name
  return `[MEMORY record_id=${record.record_id} kind=${record.kind} trust=${record.provenance.trust}] ${summary}`
}

/** The answer to `memory(record_id)`: the record itself, or nothing when it is gone, expired,
 *  outside the query's scope or not one of the kinds asked for. A fetch is a point read, so a
 *  found record carries no ranking — the kernel refuses any record but the one it named. */
export function memoryFetchRecall(
  record: MemoryRecord | null,
  query: Pick<MemoryQuery, "scope" | "kinds">,
  nowMs: number,
): MemoryRecall[] {
  if (!record) return []
  if (!memoryScopeMatches(record.scope, query.scope)) return []
  if (query.kinds.length > 0 && !query.kinds.includes(record.kind)) return []
  if (memoryRecordExpired(record, nowMs)) return []
  return [{ record, score: 1, why: "record_id" }]
}
