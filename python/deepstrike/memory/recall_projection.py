"""M5 · how a recalled record enters the model's context.

Recall pushes an index line, not the record: the header names the record, and the body is its
one-line description (its name when it has none). The model reads a record in full with
``memory(record_id)`` when it judges it relevant, so a prefetch costs a line per hit, not a record
per hit. The turn-1 prefetch lands every accepted hit in ``history`` as its own ordinary user turn —
single-use retrieval content that decays with the compression pyramid
(docs/guides/context-engineering.md, Level 5). A renewal re-query cannot append to the history of a
running operation, so its hits go to keyed knowledge ``memory:<record_id>`` instead, carrying the
same text. One rule in every SDK, pinned by the same vectors in each SDK's test.
"""

from __future__ import annotations

from deepstrike.memory.lifecycle import memory_record_expired, memory_scope_matches
from deepstrike.memory.protocols import MemoryQuery, MemoryRecall, MemoryRecord


def memory_recall_turn(record: MemoryRecord) -> str:
    """The index line one recalled record becomes.

    The header names the record and its trust level so the model can tell host-verified facts from
    untrusted ones; the retrieval score is left out because it changes between retrievals while
    the record does not.
    """
    summary = record.description.strip() or record.name
    return (f"[MEMORY record_id={record.record_id} kind={record.kind} "
            f"trust={record.provenance.trust}] {summary}")


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
