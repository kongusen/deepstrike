import pytest

from deepstrike import (
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    RuntimeSignal,
    SignalClaim,
)
from deepstrike.providers.stream import TextDelta


class _TextProvider:
    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="done")


class _RecordingLeasedSource:
    def __init__(self, ack_succeeds=True):
        self.ack_succeeds = ack_succeeds
        self.claimed = False
        self.acked = []
        self.nacked = []

    async def next_signal(self, recipient=None):
        raise RuntimeError("legacy destructive pull must not be used")

    async def claim_signal(self, recipient=None, lease_ms=None):
        if self.claimed:
            return None
        self.claimed = True
        return SignalClaim(
            delivery_id="delivery-1",
            lease_token="lease-1",
            signal=RuntimeSignal(kind="external", payload={"goal": "leased"}, source="gateway"),
            lease_expires_at_ms=30_000,
        )

    async def ack_signal(self, receipt):
        self.acked.append(receipt)
        return self.ack_succeeds

    async def nack_signal(self, receipt):
        self.nacked.append(receipt)
        return True


def _runner(source):
    return RuntimeRunner(RuntimeOptions(
        provider=_TextProvider(),
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        signal_source=source,
        max_tokens=2048,
        max_turns=2,
    ))


@pytest.mark.asyncio
async def test_runner_acks_only_after_kernel_accepts_claimed_signal():
    source = _RecordingLeasedSource()

    async for _ in _runner(source).run(session_id="leased", goal="work"):
        pass

    assert len(source.acked) == 1
    assert source.nacked == []


@pytest.mark.asyncio
async def test_runner_nacks_and_surfaces_error_when_ack_loses_lease():
    source = _RecordingLeasedSource(ack_succeeds=False)
    events = []

    async for event in _runner(source).run(session_id="lease-lost", goal="work"):
        events.append(event)

    assert len(source.acked) == 1
    assert len(source.nacked) == 1
    assert any(event.type == "error" and "signal lease" in event.message for event in events)
