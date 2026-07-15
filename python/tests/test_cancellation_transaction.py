import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.stream import TextDelta


class _LongStreamProvider:
    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="first")
        for _ in range(1000):
            yield TextDelta(delta="later")


@pytest.mark.asyncio
@pytest.mark.parametrize("reason", ["user", "deadline", "lease_lost", "host_shutdown"])
async def test_interrupt_commits_correlated_cancellation(reason):
    log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_LongStreamProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=3,
    ))

    async for event in runner.run(goal="cancel me", session_id=f"cancel-{reason}"):
        if isinstance(event, TextDelta):
            runner.interrupt(reason)

    entries = await log.read(f"cancel-{reason}")
    cancellation = next(entry.event for entry in entries if entry.event["kind"] == "operation_cancelled")
    assert cancellation["reason"] == reason
    assert len(cancellation["pending_call_ids"]) == 1
