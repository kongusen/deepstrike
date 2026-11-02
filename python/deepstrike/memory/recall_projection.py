"""M5 · the host half of reading one record in full.

Recall itself is the kernel's: it queries for the goal at run start and after each renewal and
lands an index line per hit as keyed knowledge ``memory:<record_id>``. The model reads a record with
``memory(record_id)``, answered here.
"""

from __future__ import annotations

from deepstrike.memory.lifecycle import memory_record_expired, memory_scope_matches
from deepstrike.memory.protocols import MemoryQuery, MemoryRecall, MemoryRecord


def memory_fetch_recall(record: MemoryRecord | None, query: MemoryQuery, now_ms: int) -> list[MemoryRecall]:
    """The answer to ``memory(record_id)``: the record itself, or nothing.

    Nothing when the record is gone, expired, outside the query's scope or not one of the kinds
    asked for. A fetch is a point read, so a found record carries no ranking — the kernel refuses
    any record but the one it named.
    """
    if record is None:
        return []
    if not memory_scope_matches(record.scope, query.scope):
        return []
    if query.kinds and record.kind not in query.kinds:
        return []
    if memory_record_expired(record, now_ms):
        return []
    return [MemoryRecall(record=record, score=1, why="record_id")]
