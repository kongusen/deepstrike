"""Public agent-bound durable memory adapter.

This contract is intentionally separate from ``WorkingMemory`` (an SDK scratch pad) and from
``MemoryStore`` (the runner's session/extraction protocol). It does not enter Kernel wire state.
"""
from __future__ import annotations

from deepstrike.memory.protocols import (
    MemoryQuery,
    MemoryRecord,
    MemoryScope,
    MemorySearchOptions,
    MemoryStore,
)


class DurableMemory:
    def __init__(self, store: MemoryStore, agent_id: str, scope: MemoryScope) -> None:
        self._store = store
        self._agent_id = agent_id
        self._scope = scope
        self.namespace: str | None = scope.namespace

    async def search(
        self,
        query: str,
        options: MemorySearchOptions | None = None,
        *,
        top_k: int | None = None,
    ) -> list[MemoryRecord]:
        resolved = options or MemorySearchOptions()
        request = MemoryQuery(
            scope=self._scope,
            query=query,
            top_k=top_k if top_k is not None else resolved.top_k,
            kinds=resolved.kinds,
            min_score=resolved.min_score,
        )
        return [
            hit.record
            for hit in await self._store.search(self._agent_id, request)
            if hit.record.scope == self._scope
        ]

    async def get(self, record_id: str) -> MemoryRecord | None:
        record = await self._store.get(self._agent_id, record_id)
        return record if record is not None and record.scope == self._scope else None

    async def put(self, record: MemoryRecord) -> None:
        if record.scope != self._scope:
            raise ValueError("memory record scope must match the bound Memory scope")
        await self._store.put(self._agent_id, record)

    async def delete(self, record_id: str) -> None:
        if await self.get(record_id) is not None:
            await self._store.delete(self._agent_id, record_id)
