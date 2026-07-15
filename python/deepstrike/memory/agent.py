from __future__ import annotations

from deepstrike.memory.protocols import MemoryQuery, MemoryRecall, MemoryRecord
from deepstrike.memory.ranking import rank_memories


async def select_memories(query: MemoryQuery, records: list[MemoryRecord]) -> list[MemoryRecall]:
  candidates = [record for record in records
                if record.scope == query.scope
                and (not query.kinds or record.kind in query.kinds)
                and (query.min_score is None or record.confidence >= query.min_score)]
  selected = rank_memories(
    query.query,
    candidates,
    query.top_k,
    searchable_text=lambda record: f"{record.name} {record.description} {record.content}",
    updated_at=lambda record: record.updated_at,
  )
  return [MemoryRecall(
    record=record,
    score=max(0.0, min(1.0, record.confidence)),
    why="deterministic lexical relevance with recency tie-breaking",
  ) for record in selected]
