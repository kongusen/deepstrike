from __future__ import annotations
import json
import time
from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable


@dataclass
class RuntimeSignal:
    source: str
    signal_type: str
    urgency: str
    payload: dict[str, Any] = field(default_factory=dict)
    dedupe_key: str | None = None
    # Target a specific session loop. None means a shared item consumed by one eligible puller.
    recipient: str | None = None
    # Absolute journal-clock deadline for optional urgency escalation.
    deadline_ms: int | None = None
    # Merge with an unconsumed queued signal carrying the same key.
    coalesce_key: str | None = None
    # Number of host signals deterministically represented by this signal.
    coalesced_count: int = 1

    def to_kernel_signal(self):
        from deepstrike._kernel import RuntimeSignal as KernelRuntimeSignal

        summary = str(self.payload.get("goal") or "signal")
        return KernelRuntimeSignal(
            self.source,
            self.urgency,
            summary,
            self.signal_type,
            json.dumps(self.payload),
            self.dedupe_key,
            float(int(time.time() * 1000)),
            self.recipient,
            float(self.deadline_ms) if self.deadline_ms is not None else None,
            self.coalesce_key,
            max(1, self.coalesced_count),
        )


@runtime_checkable
class SignalSource(Protocol):
    """Delivery-aware signal source used by RuntimeRunner."""
    async def claim_signal(
        self, recipient: str | None = None, lease_ms: int | None = None,
    ) -> SignalClaim | None: ...
    async def ack_signal(self, receipt: SignalDeliveryReceipt) -> bool: ...
    async def nack_signal(self, receipt: SignalDeliveryReceipt) -> bool: ...


@dataclass(frozen=True)
class SignalDeliveryReceipt:
    """Opaque proof that one consumer currently owns a leased signal delivery."""
    delivery_id: str
    lease_token: str


@dataclass(frozen=True)
class SignalClaim(SignalDeliveryReceipt):
    signal_id: str
    delivery_attempt: int
    signal: RuntimeSignal
    lease_expires_at_ms: int
