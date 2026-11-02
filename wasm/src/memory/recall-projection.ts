// M5 · the host half of reading one record in full. Recall itself is the kernel's: it queries for
// the goal at run start and after each renewal and lands an index line per hit as keyed knowledge
// `memory:<record_id>`. The model reads a record with `memory(record_id)`, answered here.

import { memoryRecordExpired } from "./lifecycle.js"
import type { MemoryQuery, MemoryRecall, MemoryRecord } from "./index.js"

/** The answer to `memory(record_id)`: the record itself, or nothing when it is gone, expired,
 *  outside the query's scope or not one of the kinds asked for. A fetch is a point read, so a
 *  found record carries no ranking — the kernel refuses any record but the one it named. */
export function memoryFetchRecall(
  record: MemoryRecord | null,
  query: Pick<MemoryQuery, "scope" | "kinds">,
  nowMs: number,
): MemoryRecall[] {
  if (!record) return []
  if (record.scope.tenant_id !== query.scope.tenant_id || record.scope.namespace !== query.scope.namespace) return []
  if (query.kinds.length > 0 && !query.kinds.includes(record.kind)) return []
  if (memoryRecordExpired(record, nowMs)) return []
  return [{ record, score: 1, why: "record_id" }]
}
