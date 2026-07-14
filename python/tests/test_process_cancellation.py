from pathlib import Path

import pytest

from deepstrike import OperationContext
from deepstrike.runtime import ProcessSandboxPlane, RunContext
from deepstrike._kernel import ToolCall


@pytest.mark.asyncio
async def test_sandbox_subprocess_stops_when_operation_is_already_cancelled(tmp_path: Path):
    plane = ProcessSandboxPlane(sandbox_dir=tmp_path, timeout_ms=30_000)
    operation = OperationContext(run_id="run", session_id="session")
    operation.cancelled.set()
    events = []

    async for event in plane.execute_all([
        ToolCall(id="call", name="run_python", arguments='{"code":"import time; time.sleep(10)"}'),
    ], RunContext(operation=operation)):
        events.append(event)

    assert len(events) == 1
    assert "operation cancelled" in events[0].content
