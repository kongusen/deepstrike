"""`InMemoryDreamStore` — lightweight `DreamStore` backed by per-agent dicts.

Python port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests,
and local development where persistent memory isn't needed.
"""
from __future__ import annotations

from typing import Iterable

from deepstrike.memory.protocols import DreamStore, MemoryQuery, MemoryRecall, MemoryRecord, SessionData
from deepstrike.memory.ranking import rank_memories


class InMemoryDreamStore(DreamStore):
    def __init__(self, initial_memories: Iterable[MemoryRecord] | None = None) -> None:
        self._memories: dict[str, list[MemoryRecord]] = {}
        self._initial: list[MemoryRecord] = list(initial_memories or [])
        #: Sessions persisted via save_session — exposed for test assertions.
        self.saved_sessions: list[SessionData] = []

    def _records_for(self, agent_id: str) -> list[MemoryRecord]:
        if agent_id in self._memories:
            return list(self._memories[agent_id])
        if self._initial:
            self._memories[agent_id] = list(self._initial)
            return list(self._memories[agent_id])
        return []

    async def upsert(self, agent_id: str, incoming: MemoryRecord) -> None:
        kept = self._records_for(agent_id)
        index = next((i for i, record in enumerate(kept) if record.scope == incoming.scope and record.kind == incoming.kind and record.name == incoming.name), None)
        if index is None:
            kept.append(incoming)
        else:
            kept[index] = incoming
        self._memories[agent_id] = kept

    async def search(self, agent_id: str, query: MemoryQuery) -> list[MemoryRecall]:
        candidates = [record for record in self._records_for(agent_id)
                      if record.scope == query.scope
                      and (not query.kinds or record.kind in query.kinds)
                      and (query.min_score is None or record.confidence >= query.min_score)]
        selected = rank_memories(
            query.query, candidates, query.top_k,
            searchable_text=lambda record: f"{record.name} {record.description} {record.content}",
            updated_at=lambda record: record.updated_at,
        )
        return [MemoryRecall(record=record, score=max(0.0, min(1.0, record.confidence)), why="deterministic lexical relevance with recency tie-breaking") for record in selected]

    async def save_session(self, data: SessionData) -> None:
        self.saved_sessions.append(data)
