from __future__ import annotations
from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class KnowledgeSource(Protocol):
    """On-demand knowledge retrieval, exposed to the LLM as a `knowledge` tool.
    The LLM queries this based on context; results are returned as tool results into history."""
    async def retrieve(self, goal: str, top_k: int = 5) -> list[str]:
        """Return a list of relevant text snippets for the given goal."""
        ...

    async def init(self) -> None:
        """One-time warmup called before the first run (load index, open connection, etc.)."""
        ...
