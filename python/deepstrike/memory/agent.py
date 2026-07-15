from __future__ import annotations

from deepstrike.memory.protocols import MemoryQuery, MemoryRecall, MemoryRecord
from deepstrike.memory.ranking import rank_memories


async def select_memories(query: MemoryQuery, records: list[MemoryRecord]) -> list[MemoryRecall]:
  candidates = [record for record in records
                if record.scope == query.scope
                and (not query.kinds or record.kind in query.kinds)]
  ranked = rank_memories(
    query.query,
    candidates,
    query.top_k,
    searchable_text=lambda record: f"{record.name} {record.description} {record.content}",
    updated_at=lambda record: record.updated_at,
    recall_count=lambda record: record.recall_count,
    ttl_days=lambda record: record.ttl_days,
  )
  # score is relevance (from ranking), deliberately distinct from the record's stored confidence.
  return [MemoryRecall(record=hit.value, score=hit.score, why=hit.why)
          for hit in ranked
          if query.min_score is None or hit.score >= query.min_score]
