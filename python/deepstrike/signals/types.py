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
    # Optional pub/sub topic (carried through; multi-subscriber routing deferred).
    topic: str | None = None

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
            self.topic,
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
