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


@dataclass(frozen=True)
class MemorySearchOptions:
    top_k: int = 5
    kinds: list[MemoryKind] = field(default_factory=list)
    min_score: float | None = None


@runtime_checkable
class Memory(Protocol):
    """Public durable memory bound to one agent and scope, distinct from ``WorkingMemory``."""

    namespace: str | None

    async def search(self, query: str, options: MemorySearchOptions | None = None) -> list[MemoryRecord]: ...
    async def get(self, record_id: str) -> MemoryRecord | None: ...
    async def put(self, record: MemoryRecord) -> None: ...
    async def delete(self, record_id: str) -> None: ...


@runtime_checkable
class MemoryStore(Protocol):
    """Host-owned storage protocol behind an agent-bound public ``Memory`` adapter."""

    async def put(self, agent_id: str, record: MemoryRecord) -> None: ...
    async def get(self, agent_id: str, record_id: str) -> MemoryRecord | None: ...
    async def delete(self, agent_id: str, record_id: str) -> None: ...
    async def search(self, agent_id: str, query: "MemoryQuery") -> list[MemoryRecall]: ...
    async def save_session(self, data: "SessionData") -> None: ...
    async def record_recall(self, agent_id: str, recalls: list[MemoryRecallLifecycle]) -> None: ...
    async def set_pinned(self, agent_id: str, record_id: str, pinned: bool) -> None: ...



@dataclass
class SessionData:
    session_id: str
    agent_id: str
    """Message objects using the kernel message contract."""
    messages: list["Message"]
    metadata: Any = None
    created_at_ms: int = 0
    updated_at_ms: int = 0
