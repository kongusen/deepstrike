import asyncio

import pytest

from deepstrike.runtime.reliability import ManagedTaskScope, OperationContext


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
