import pytest
from deepstrike import (
    InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner,
    MilestoneContract, MilestonePhase, MilestoneCheckResult,
)
from deepstrike.providers.stream import TextDelta

class FakeProvider:
    def __init__(self):
        self.call_count = 0

    async def stream(self, context, tools, extensions=None, state=None):
        self.call_count += 1
        yield TextDelta(delta="done")

@pytest.mark.asyncio
async def test_milestone_auto_pass():
    provider = FakeProvider()
    contract = MilestoneContract(phases=[
        MilestonePhase(id="phase1", criteria=["must complete"])
    ])

    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        milestone_contract=contract,
        milestone_policy="auto_pass",
        max_tokens=1000,
    ))

    events = []
    async for evt in runner.run(goal="test", session_id="s_auto"):
        events.append(evt)

    done_evts = [e for e in events if getattr(e, "type", None) == "done"]
    assert len(done_evts) == 1
    assert done_evts[0].status == "completed"

@pytest.mark.asyncio
async def test_milestone_pending_by_default():
    provider = FakeProvider()
    contract = MilestoneContract(phases=[
        MilestonePhase(id="phase1", criteria=["must complete"])
    ])

    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        milestone_contract=contract,
        milestone_policy="require_verifier",
        max_tokens=1000,
    ))

    events = []
    async for evt in runner.run(goal="test", session_id="s_pending"):
        events.append(evt)

    done_evts = [e for e in events if getattr(e, "type", None) == "done"]
    assert len(done_evts) == 1
    assert done_evts[0].status == "milestone_pending"

@pytest.mark.asyncio
async def test_milestone_verifier_callback():
    provider = FakeProvider()
    contract = MilestoneContract(phases=[
        MilestonePhase(id="phase1", criteria=["must complete"])
    ])

    called_verifier = []

    async def verifier(ctx):
        called_verifier.append(ctx)
        return MilestoneCheckResult(phase_id=ctx["phaseId"], passed=True)

    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=InMemorySessionLog(),
        execution_plane=LocalExecutionPlane(),
        milestone_contract=contract,
        on_milestone_evaluate=verifier,
        max_tokens=1000,
    ))

    events = []
    async for evt in runner.run(goal="test", session_id="s_verify"):
        events.append(evt)

    assert len(called_verifier) == 1
    assert called_verifier[0]["phaseId"] == "phase1"

    done_evts = [e for e in events if getattr(e, "type", None) == "done"]
    assert len(done_evts) == 1
    assert done_evts[0].status == "completed"
