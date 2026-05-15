from __future__ import annotations
import json
import time
from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable


@dataclass
class RuntimeSignal:
    kind: str
    payload: dict[str, Any] = field(default_factory=dict)
    source: str = "custom"
    signal_type: str = "event"
    urgency: str = "normal"
    dedupe_key: str | None = None
    priority: int | None = None

    def __post_init__(self) -> None:
        # Preserve the old ergonomic kinds while normalizing onto the kernel contract.
        if self.kind == "scheduled":
            self.source = "cron"
            self.signal_type = "job"
        elif self.kind == "interrupt":
            self.source = "gateway"
            self.signal_type = "alert"
            if self.urgency == "normal":
                self.urgency = "critical"

        if self.priority is not None and self.urgency == "normal":
            if self.priority >= 10:
                self.urgency = "critical"
            elif self.priority >= 5:
                self.urgency = "high"
            elif self.priority < 0:
                self.urgency = "low"

    def to_kernel_signal(self):
        from deepstrike._kernel import RuntimeSignal as KernelRuntimeSignal

        summary = str(self.payload.get("goal") or self.kind)
        return KernelRuntimeSignal(
            self.source,
            self.urgency,
            summary,
            self.signal_type,
            json.dumps(self.payload),
            self.dedupe_key,
            float(int(time.time() * 1000)),
        )


@runtime_checkable
class SignalSource(Protocol):
    """Implement this to feed signals into an Agent from any external source."""
    async def next_signal(self) -> RuntimeSignal | None:
        """Return the next pending signal, or None if none available."""
        ...
