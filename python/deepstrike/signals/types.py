from __future__ import annotations
from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable


@dataclass
class RuntimeSignal:
    kind: str          # "interrupt" | "scheduled" | "external"
    payload: dict[str, Any] = field(default_factory=dict)
    priority: int = 0


@runtime_checkable
class SignalSource(Protocol):
    """Implement this to feed signals into an Agent from any external source."""
    async def next_signal(self) -> RuntimeSignal | None:
        """Return the next pending signal, or None if none available."""
        ...
