from __future__ import annotations
import asyncio
import time
from collections import deque
from typing import Callable
from deepstrike.signals.types import RuntimeSignal
from deepstrike.signals.scheduled import ScheduledPrompt


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
        gateway.ingest(RuntimeSignal(kind="external", payload={"foo": "bar"}))
        # call gateway.destroy() when done to cancel pending timers
    """

    def __init__(self) -> None:
        self._listeners: list[Callable[[RuntimeSignal], None]] = []
        self._tasks: dict[str, asyncio.Task] = {}
        self._pending: deque[RuntimeSignal] = deque()

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

    async def next_signal(self, recipient: str | None = None) -> RuntimeSignal | None:
        """Return the next pending signal so the gateway can be passed directly to RuntimeRunner.

        When ``recipient`` is given, return only the oldest signal addressed to it (plus
        unaddressed broadcasts); signals addressed to other recipients stay queued, so one
        shared gateway can serve N peer loops. None ⇒ legacy FIFO drain (any signal).
        """
        if recipient is None:
            return self._pending.popleft() if self._pending else None
        for i, sig in enumerate(self._pending):
            if sig.recipient is None or sig.recipient == recipient:
                del self._pending[i]
                return sig
        return None

    def destroy(self) -> None:
        """Cancel all pending scheduled tasks."""
        for task in list(self._tasks.values()):
            task.cancel()
        self._tasks.clear()

    def _emit(self, signal: RuntimeSignal) -> None:
        self._pending.append(signal)
        for listener in self._listeners:
            listener(signal)
