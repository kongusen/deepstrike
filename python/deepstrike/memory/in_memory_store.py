"""`InMemoryDreamStore` — lightweight `DreamStore` backed by per-agent dicts.

Python port of node/src/memory/in-memory-store.ts. Use for benchmarks, unit tests,
and local development where persistent memory isn't needed.
"""
from __future__ import annotations

from typing import Iterable

from deepstrike.memory.protocols import CurationResult, DreamStore, MemoryEntry, SessionData


class InMemoryDreamStore(DreamStore):
    def __init__(self, initial_memories: Iterable[MemoryEntry] | None = None) -> None:
        self._sessions: dict[str, list[SessionData]] = {}
        self._memories: dict[str, list[MemoryEntry]] = {}
        self._initial: list[MemoryEntry] = list(initial_memories or [])
        #: Sessions persisted via save_session — exposed for test assertions.
        self.saved_sessions: list[SessionData] = []

    def add_session(self, agent_id: str, session: SessionData) -> None:
        self._sessions.setdefault(agent_id, []).append(session)

    def add_memories(self, agent_id: str, entries: Iterable[MemoryEntry]) -> None:
        self._memories.setdefault(agent_id, []).extend(entries)

    async def load_sessions(self, agent_id: str) -> list[SessionData]:
        return list(self._sessions.get(agent_id, []))

    async def load_memories(self, agent_id: str) -> list[MemoryEntry]:
        if agent_id in self._memories:
            return list(self._memories[agent_id])
        if self._initial:
            self._memories[agent_id] = list(self._initial)
            return list(self._memories[agent_id])
        return []

    async def commit(
        self,
        agent_id: str,
        result: CurationResult,
        existing: list[MemoryEntry],
    ) -> None:
        kept = [m for i, m in enumerate(existing) if i not in set(result.to_remove_indices)]
        self._memories[agent_id] = kept + list(result.to_add)

    async def search(self, agent_id: str, query: str, top_k: int = 5) -> list[MemoryEntry]:
        all_memories = await self.load_memories(agent_id)
        return all_memories[:top_k]

    async def save_session(self, data: SessionData) -> None:
        self.saved_sessions.append(data)
        self._sessions.setdefault(data.agent_id, []).append(data)
