import asyncio

import pytest

from deepstrike.runtime.reliability import ManagedTaskScope, OperationContext, run_with_operation


def _operation() -> OperationContext:
    return OperationContext(run_id="run-1", session_id="session-1", agent_id="agent-1")


@pytest.mark.asyncio
async def test_managed_task_scope_drains_owned_work():
    completed = []
    scope = ManagedTaskScope(_operation())

    async def persist():
        await asyncio.sleep(0)
        completed.append("persisted")

    scope.spawn("persist-summary", persist())
    await scope.drain()

    assert completed == ["persisted"]
    assert scope.pending == 0


@pytest.mark.asyncio
async def test_managed_task_scope_reports_failure_with_operation_identity():
    failures = []
    scope = ManagedTaskScope(_operation(), failures.append)

    async def fail():
        raise RuntimeError("store unavailable")

    scope.spawn("semantic-page-out", fail())
    await scope.drain()

    assert [(failure.label, failure.operation.run_id) for failure in failures] == [
        ("semantic-page-out", "run-1")
    ]


@pytest.mark.asyncio
async def test_managed_task_scope_rejects_work_after_close():
    scope = ManagedTaskScope(_operation())
    await scope.drain()

    async def late():
        return None

    coroutine = late()
    with pytest.raises(RuntimeError, match="task scope is closed"):
        scope.spawn("late", coroutine)
    coroutine.close()


@pytest.mark.asyncio
async def test_run_with_operation_cancels_adapter_work():
    operation = _operation()

    async def work():
        await asyncio.sleep(10)

    operation.cancelled.set()
    with pytest.raises(asyncio.CancelledError):
        await run_with_operation(work(), operation, timeout_ms=30_000)


@pytest.mark.asyncio
async def test_run_with_operation_honors_expired_deadline():
    operation = OperationContext(run_id="run", session_id="session", deadline_ms=0)

    async def work():
        await asyncio.sleep(10)

    with pytest.raises(TimeoutError):
        await run_with_operation(work(), operation, timeout_ms=30_000)
