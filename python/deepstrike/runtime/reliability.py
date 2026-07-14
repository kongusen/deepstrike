from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass
from types import MappingProxyType
from typing import Any, Awaitable, Callable, Mapping


@dataclass(frozen=True)
class ObserverFailure:
    component: str
    operation: str
    cause: BaseException
    committed: bool = True


ObserverErrorHandler = Callable[[ObserverFailure], Any]


@dataclass(frozen=True)
class OperationContext:
    run_id: str
    session_id: str
    agent_id: str | None = None
    cancelled: asyncio.Event | None = None
    deadline_ms: int | None = None
    provenance: Mapping[str, str] | None = None

    def __post_init__(self) -> None:
        if self.cancelled is None:
            object.__setattr__(self, "cancelled", asyncio.Event())
        if self.provenance is not None and not isinstance(self.provenance, MappingProxyType):
            object.__setattr__(self, "provenance", MappingProxyType(dict(self.provenance)))


@dataclass(frozen=True)
class BackgroundTaskFailure:
    label: str
    operation: OperationContext
    cause: BaseException


BackgroundTaskErrorHandler = Callable[[BackgroundTaskFailure], Any]


async def run_with_operation(
    work: Awaitable[Any],
    operation: OperationContext | None,
    *,
    timeout_ms: int,
) -> Any:
    """Run adapter work under the earliest local timeout, operation deadline, or cancellation."""
    timeout_s = timeout_ms / 1000
    if operation is not None and operation.deadline_ms is not None:
        timeout_s = min(timeout_s, (operation.deadline_ms - int(time.time() * 1000)) / 1000)
    task = asyncio.ensure_future(work)
    cancel_waiter: asyncio.Task[bool] | None = None
    try:
        if operation is not None and operation.cancelled is not None:
            cancel_waiter = asyncio.create_task(operation.cancelled.wait())
        waiters = {task, *([cancel_waiter] if cancel_waiter is not None else [])}
        done, _ = await asyncio.wait(waiters, timeout=max(0, timeout_s), return_when=asyncio.FIRST_COMPLETED)
        if task in done:
            return await task
        task.cancel()
        await asyncio.gather(task, return_exceptions=True)
        if cancel_waiter is not None and cancel_waiter in done:
            raise asyncio.CancelledError("operation cancelled")
        raise TimeoutError("operation deadline exceeded")
    finally:
        if cancel_waiter is not None:
            cancel_waiter.cancel()
            await asyncio.gather(cancel_waiter, return_exceptions=True)


def report_observer_failure(
    handler: ObserverErrorHandler | None,
    *,
    component: str,
    operation: str,
    cause: BaseException,
) -> None:
    """Report an observer failure without changing the already committed result."""
    if handler is None:
        return
    try:
        handler(ObserverFailure(component=component, operation=operation, cause=cause))
    except Exception:
        # The reporter is itself an observer; it cannot become a second semantic owner.
        pass


class ManagedTaskScope:
    """Own best-effort asynchronous work for exactly one operation."""

    def __init__(
        self,
        operation: OperationContext,
        on_task_error: BackgroundTaskErrorHandler | None = None,
    ) -> None:
        self.operation = operation
        self._on_task_error = on_task_error
        self._tasks: set[asyncio.Task[None]] = set()
        self._closed = False

    @property
    def pending(self) -> int:
        return len(self._tasks)

    def spawn(self, label: str, work: Awaitable[None]) -> None:
        if self._closed:
            raise RuntimeError("task scope is closed")

        async def run() -> None:
            try:
                await work
            except asyncio.CancelledError:
                raise
            except Exception as cause:
                if self._on_task_error is not None:
                    try:
                        self._on_task_error(BackgroundTaskFailure(label, self.operation, cause))
                    except Exception:
                        pass

        task = asyncio.create_task(run())
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)

    async def drain(self) -> None:
        self._closed = True
        if self._tasks:
            await asyncio.gather(*tuple(self._tasks), return_exceptions=True)

    async def cancel(self) -> None:
        self._closed = True
        assert self.operation.cancelled is not None
        self.operation.cancelled.set()
        for task in tuple(self._tasks):
            task.cancel()
        if self._tasks:
            await asyncio.gather(*tuple(self._tasks), return_exceptions=True)
