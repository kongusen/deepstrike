from __future__ import annotations
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

if TYPE_CHECKING:
    from deepstrike._kernel import Message


# ─── Dream / idle-pipeline types ─────────────────────────────────────────────

@dataclass
class MemoryEntry:
    text: str
    score: float = 0.0
    metadata: Any = None


@dataclass
class CurationStats:
    insights_processed: int = 0
    duplicates_removed: int = 0
    conflicts_resolved: int = 0
    entries_added: int = 0


@dataclass
class CurationResult:
    to_add: list[MemoryEntry] = field(default_factory=list)
    """Indices into the `existing` list passed to `DreamStore.commit`."""
    to_remove_indices: list[int] = field(default_factory=list)
    stats: CurationStats = field(default_factory=CurationStats)


@dataclass
class SessionData:
    session_id: str
    agent_id: str
    """Message objects using the kernel message contract."""
    messages: list["Message"]
    metadata: Any = None
    created_at_ms: int = 0
    updated_at_ms: int = 0


@dataclass
class DreamResult:
    sessions_processed: int = 0
    insights_extracted: int = 0
    entries_added: int = 0
    entries_removed: int = 0


@runtime_checkable
class DreamStore(Protocol):
    """Backing store for the idle dreaming pipeline."""

    async def load_sessions(self, agent_id: str) -> list[SessionData]:
        """Return recent sessions for the given agent."""
        ...

    async def load_memories(self, agent_id: str) -> list[MemoryEntry]:
        """Return all current long-term memory entries for the agent."""
        ...

    async def commit(
        self,
        agent_id: str,
        result: CurationResult,
        existing: list[MemoryEntry],
    ) -> None:
        """Apply the curation delta — add new entries, remove stale ones."""
        ...

    async def search(self, agent_id: str, query: str, top_k: int = 5) -> list[MemoryEntry]:
        """Semantic search over the agent's long-term memories. Called on demand during a run."""
        ...

    async def save_session(self, data: "SessionData") -> None:
        """Persist a completed session for future consolidation via `Agent.dream()`."""
        ...


@runtime_checkable
class SessionStore(Protocol):
    """Durable transcript storage for same-session conversational continuity."""

    async def load_session(self, session_id: str) -> SessionData | None:
        ...

    async def save_session(self, data: SessionData) -> None:
        ...
