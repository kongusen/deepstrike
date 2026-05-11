from __future__ import annotations
from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class KnowledgeSource(Protocol):
    """Provides run-scoped evidence injected into C_working before the first LLM call."""
    async def retrieve(self, goal: str, top_k: int = 5) -> list[str]:
        """Return a list of relevant text snippets for the given goal."""
        ...
