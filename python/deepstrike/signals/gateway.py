from __future__ import annotations
import asyncio
import time
import uuid
from collections import deque
from dataclasses import dataclass, replace
from typing import Callable
from deepstrike.signals.types import RuntimeSignal, SignalClaim, SignalDeliveryReceipt
from deepstrike.signals.scheduled import ScheduledPrompt
from deepstrike.runtime.reliability import ObserverErrorHandler, report_observer_failure


@dataclass
class _QueuedSignal:
    delivery_id: str
    signal_id: str
    delivery_attempts: int
    signal: RuntimeSignal
    lease_token: str | None = None
    lease_expires_at_ms: int | None = None


class SignalGateway:
    """
    Entry point for all external signals into the agent.

    Responsibilities:
    - Cron scheduling: fires ScheduledPrompts at the right time (deduplicated by goal+run_at_ms)
    - Webhook ingestion: converts raw external payloads to RuntimeSignal
    - Listener dispatch: notifies registered callbacks on each signal

    Usage::

        gateway = SignalGateway()
        gateway.on_signal(lambda sig: ...)
        gateway.schedule(ScheduledPrompt("summarize", int(time.time() * 1000) + 60_000))
        gateway.ingest(RuntimeSignal(
            source="gateway", signal_type="event", urgency="normal", payload={"foo": "bar"},
        ))
        # call gateway.destroy() when done to cancel pending timers
    """

    def __init__(
        self, *, on_observer_error: ObserverErrorHandler | None = None,
        now: Callable[[], int] | None = None, default_lease_ms: int = 30_000,
    ) -> None:
        if default_lease_ms <= 0:
            raise ValueError("default_lease_ms must be positive")
        self._listeners: list[Callable[[RuntimeSignal], None]] = []
        self._tasks: dict[str, asyncio.Task] = {}
        self._pending: deque[_QueuedSignal] = deque()
        self._on_observer_error = on_observer_error
        self._now = now or (lambda: int(time.time() * 1000))
        self._default_lease_ms = default_lease_ms
        self._delivery_seq = 0
        self._lease_seq = 0

    def on_signal(self, listener: Callable[[RuntimeSignal], None]) -> Callable[[], None]:
        """Register a listener that is called synchronously whenever a signal is emitted.

        Returns an unsubscribe function — long-lived consumers (e.g. a loop's
        ``signal_aware_sleeper``, re-registered per sleep) must call it or the listener leaks.
        """
        self._listeners.append(listener)

        def _unsubscribe() -> None:
            try:
                self._listeners.remove(listener)
            except ValueError:
                pass

        return _unsubscribe

    def schedule(self, prompt: ScheduledPrompt) -> None:
        """Schedule a ScheduledPrompt to fire at its run_at_ms. Idempotent by goal+time."""
        key = f"cron:{prompt.goal}:{prompt.run_at_ms}"
        if key in self._tasks:
            return

        signal = prompt.to_signal()
        delay_s = (prompt.run_at_ms - int(time.time() * 1000)) / 1000.0

        async def _fire(k: str) -> None:
            if delay_s > 0:
                await asyncio.sleep(delay_s)
            self._tasks.pop(k, None)
            self._emit(signal)

        try:
            loop = asyncio.get_event_loop()
            self._tasks[key] = loop.create_task(_fire(key))
        except RuntimeError:
            # No running event loop — fire immediately (best-effort for sync contexts)
            self._emit(signal)

    def cancel(self, goal: str, run_at_ms: int) -> None:
        """Cancel a previously scheduled prompt."""
        key = f"cron:{goal}:{run_at_ms}"
        task = self._tasks.pop(key, None)
        if task:
            task.cancel()

    def ingest(self, signal: RuntimeSignal) -> None:
        """Ingest a raw external signal (e.g. from a webhook handler)."""
        self._emit(signal)

    def broadcast(self, recipients: list[str], signal: RuntimeSignal) -> None:
        """Fan one logical signal out to a known recipient set."""
        seen: set[str] = set()
        for recipient in recipients:
            if not recipient or recipient in seen:
                continue
            seen.add(recipient)
            self._emit(replace(signal, recipient=recipient))

    async def claim_signal(
        self, recipient: str | None = None, lease_ms: int | None = None,
    ) -> SignalClaim | None:
        """Claim one visible signal without deleting it; expiry makes it visible again."""
        duration = self._default_lease_ms if lease_ms is None else lease_ms
        if duration <= 0:
            raise ValueError("lease_ms must be positive")
        now = self._now()
        for entry in self._pending:
            visible = (
                recipient is None
                or entry.signal.recipient is None
                or entry.signal.recipient == recipient
            )
            available = entry.lease_token is None or (entry.lease_expires_at_ms or 0) <= now
            if not visible or not available:
                continue
            self._lease_seq += 1
            entry.delivery_attempts += 1
            token = f"{entry.delivery_id}:lease-{self._lease_seq}"
            expires_at = now + duration
            entry.lease_token = token
            entry.lease_expires_at_ms = expires_at
            return SignalClaim(
                entry.delivery_id,
                token,
                entry.signal_id,
                entry.delivery_attempts,
                entry.signal,
                expires_at,
            )
        return None

    async def ack_signal(self, receipt: SignalDeliveryReceipt) -> bool:
        """Permanently remove a delivery iff the receipt owns its current lease."""
        for i, entry in enumerate(self._pending):
            if entry.delivery_id == receipt.delivery_id and entry.lease_token == receipt.lease_token:
                del self._pending[i]
                return True
        return False

    async def nack_signal(self, receipt: SignalDeliveryReceipt) -> bool:
        """Release the current lease for immediate retry; ignore stale receipts."""
        for entry in self._pending:
            if entry.delivery_id == receipt.delivery_id and entry.lease_token == receipt.lease_token:
                entry.lease_token = None
                entry.lease_expires_at_ms = None
                return True
        return False

    def destroy(self) -> None:
        """Cancel all pending scheduled tasks."""
        for task in list(self._tasks.values()):
            task.cancel()
        self._tasks.clear()

    @property
    def depth(self) -> int:
        return len(self._pending)

    def _emit(self, signal: RuntimeSignal) -> None:
        self._delivery_seq += 1
        self._pending.append(_QueuedSignal(
            f"signal-{self._delivery_seq}",
            str(uuid.uuid4()),
            0,
            signal,
        ))
        for listener in self._listeners:
            try:
                listener(signal)
            except Exception as cause:
                report_observer_failure(
                    self._on_observer_error,
                    component="SignalGateway",
                    operation="signal_listener",
                    cause=cause,
                )
