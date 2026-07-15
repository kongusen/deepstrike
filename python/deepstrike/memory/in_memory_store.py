"""`InMemoryDreamStore` — lightweight `DreamStore` backed by per-agent dicts.

Python port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests,
and local development where persistent memory isn't needed.
"""
from __future__ import annotations

import time
from typing import Callable, Iterable

from deepstrike.memory.protocols import (
    DreamStore,
    MemoryQuery,
    MemoryRecall,
    MemoryRecallLifecycle,
    MemoryRecord,
    SessionData,
)
from deepstrike.memory.ranking import rank_memories
from deepstrike.memory.retention import memory_retention_score


def _now_ms() -> int:
    return int(time.time() * 1000)


class InMemoryDreamStore(DreamStore):
    """Reference ``DreamStore``. Search returns a genuine relevance score (never stored confidence);
    the store is the authority for the full record set, so it bounds itself by value-ordered
    retention eviction (M3) and mirrors recall lifecycle and pin state (M3/M4)."""

    def __init__(
        self,
        initial_memories: Iterable[MemoryRecord] | None = None,
        *,
        max_records: int | None = None,
        stale_warning_days: int = 2,
        now: Callable[[], int] | None = None,
    ) -> None:
        self._memories: dict[str, list[MemoryRecord]] = {}
        self._initial: list[MemoryRecord] = list(initial_memories or [])
        #: Sessions persisted via save_session — exposed for test assertions.
        self.saved_sessions: list[SessionData] = []
        self._max_records = max_records
        self._stale_warning_days = stale_warning_days
        self._now = now or _now_ms

    def _records_for(self, agent_id: str) -> list[MemoryRecord]:
        if agent_id in self._memories:
            return self._memories[agent_id]
        if self._initial:
            self._memories[agent_id] = list(self._initial)
            return self._memories[agent_id]
        return []

    def _evict_to_capacity(self, records: list[MemoryRecord]) -> list[MemoryRecord]:
        """M3: value-ordered retention eviction — shed the lowest-value unpinned records until the
        set fits ``max_records``. Never a blind tail-cut."""
        if self._max_records is None or len(records) <= self._max_records:
            return records
        now_ms = self._now()
        scored = [
            (
                float("inf") if record.pinned else memory_retention_score(record, now_ms, self._stale_warning_days),
                index,
                record,
            )
            for index, record in enumerate(records)
        ]
        # Keep the highest-value max_records; ties break on insertion order (older first survives).
        scored.sort(key=lambda row: (-row[0], row[1]))
        return [record for _score, _index, record in scored[: self._max_records]]

    async def upsert(self, agent_id: str, incoming: MemoryRecord) -> None:
        kept = list(self._records_for(agent_id))
        index = next((i for i, record in enumerate(kept) if record.scope == incoming.scope and record.kind == incoming.kind and record.name == incoming.name), None)
        if index is None:
            kept.append(incoming)
        else:
            kept[index] = incoming
        self._memories[agent_id] = self._evict_to_capacity(kept)

    async def search(self, agent_id: str, query: MemoryQuery) -> list[MemoryRecall]:
        candidates = [record for record in self._records_for(agent_id)
                      if record.scope == query.scope
                      and (not query.kinds or record.kind in query.kinds)]
        ranked = rank_memories(
            query.query, candidates, query.top_k,
            searchable_text=lambda record: f"{record.name} {record.description} {record.content}",
            updated_at=lambda record: record.updated_at,
            recall_count=lambda record: record.recall_count,
            ttl_days=lambda record: record.ttl_days,
            now_ms=self._now(),
            stale_warning_days=self._stale_warning_days,
        )
        # score is relevance (from ranking), deliberately distinct from stored confidence.
        return [
            MemoryRecall(record=hit.value, score=hit.score, why=hit.why)
            for hit in ranked
            if query.min_score is None or hit.score >= query.min_score
        ]

    async def save_session(self, data: SessionData) -> None:
        self.saved_sessions.append(data)

    async def record_recall(self, agent_id: str, recalls: list[MemoryRecallLifecycle]) -> None:
        """M3: mirror the kernel's journaled recall lifecycle into the durable records."""
        records = self._records_for(agent_id)
        by_id = {recall.record_id: recall for recall in recalls}
        for record in records:
            recall = by_id.get(record.record_id)
            if recall is not None:
                record.recall_count = recall.recall_count
                record.last_recalled_at = recall.last_recalled_at

    async def set_pinned(self, agent_id: str, record_id: str, pinned: bool) -> None:
        """M4: set a record's pin so retention eviction cannot shed it."""
        for record in self._records_for(agent_id):
            if record.record_id == record_id:
                record.pinned = pinned
