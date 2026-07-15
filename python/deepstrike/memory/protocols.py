from __future__ import annotations
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Literal, Protocol, runtime_checkable

if TYPE_CHECKING:
    from deepstrike._kernel import Message


# ─── Durable-memory types ────────────────────────────────────────────────────

MemoryKind = Literal["user", "feedback", "project", "reference"]
MemoryAuthor = Literal["model", "host", "extraction"]
MemoryTrustLevel = Literal["untrusted", "user_asserted", "host_verified"]

@dataclass(frozen=True)
class MemoryScope:
    tenant_id: str
    namespace: str

@dataclass
class MemoryProvenance:
    author: MemoryAuthor
    trust: MemoryTrustLevel
    evidence_refs: list[str] = field(default_factory=list)
    session_id: str | None = None

@dataclass
class MemoryRecord:
    record_id: str
    scope: MemoryScope
    name: str
    kind: MemoryKind
    content: str
    description: str
    provenance: MemoryProvenance
    created_at: int
    updated_at: int
    last_recalled_at: int | None = None
    recall_count: int = 0
    confidence: float = 1.0
    links: list[str] = field(default_factory=list)
    pinned: bool = False
    ttl_days: int | None = None

@dataclass
class MemoryRecall:
    record: MemoryRecord
    score: float
    why: str

@dataclass
class MemoryQuery:
    scope: MemoryScope
    query: str
    top_k: int = 5
    kinds: list[MemoryKind] = field(default_factory=list)
    min_score: float | None = None

@dataclass
class MemoryRecallLifecycle:
    """One record's recall lifecycle, mirrored from the kernel's ``memory_recalled`` observation."""
    record_id: str
    recall_count: int
    last_recalled_at: int


@dataclass
class SessionData:
    session_id: str
    agent_id: str
    """Message objects using the kernel message contract."""
    messages: list["Message"]
    metadata: Any = None
    created_at_ms: int = 0
    updated_at_ms: int = 0


@runtime_checkable
class DreamStore(Protocol):
    """Durable store whose only mutation is a gated record upsert."""

    async def upsert(self, agent_id: str, record: MemoryRecord) -> None:
        """Persist one canonical record after the kernel WriteMemory gate accepts it."""
        ...

    async def search(self, agent_id: str, query: MemoryQuery) -> list[MemoryRecall]:
        """Semantic search over the agent's long-term memories. Called on demand during a run."""
        ...

    async def save_session(self, data: "SessionData") -> None:
        """Persist a completed session before the runner's one extraction pass."""
        ...

    # Optional lifecycle methods (not part of the runtime_checkable required surface so existing
    # stores keep passing isinstance). The runner calls them via getattr when present:
    #   async def record_recall(agent_id: str, recalls: list[MemoryRecallLifecycle]) -> None  # M3
    #   async def set_pinned(agent_id: str, record_id: str, pinned: bool) -> None              # M4
