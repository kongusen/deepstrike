"""
07 — SinglePassHarness, EvalLoopHarness, HarnessLoop
"""
import pytest

from deepstrike import (
    SinglePassHarness, EvalLoopHarness, HarnessLoop,
    HarnessRequest, HarnessOutcome,
)

from conftest import make_agent, make_provider


class TestSinglePassHarness:
    @pytest.mark.timeout(60)
    async def test_always_returns_passed_true(self):
        outcome = await SinglePassHarness(make_agent()).run(HarnessRequest(goal='Reply "done".'))
        assert outcome.passed is True
        assert len(outcome.result) > 0
        assert outcome.iterations >= 0
        assert outcome.total_tokens >= 0


class TestEvalLoopHarness:
    @pytest.mark.timeout(60)
    async def test_passes_on_first_attempt(self):
        class AlwaysPass:
            async def evaluate(self, request, outcome):
                return True

        outcome = await EvalLoopHarness(make_agent(), AlwaysPass(), max_attempts=3).run(
            HarnessRequest(goal='Say "hello".')
        )
        assert outcome.passed is True

    @pytest.mark.timeout(120)
    async def test_retries_then_passes(self):
        class PassOnSecond:
            def __init__(self):
                self.count = 0
            async def evaluate(self, request, outcome):
                self.count += 1
                return self.count >= 2

        gate = PassOnSecond()
        outcome = await EvalLoopHarness(make_agent(), gate, max_attempts=3).run(
            HarnessRequest(goal='Say "hello".')
        )
        assert outcome.passed is True
        assert gate.count >= 2

    @pytest.mark.timeout(120)
    async def test_returns_false_when_gate_never_passes(self):
        class NeverPass:
            async def evaluate(self, request, outcome):
                return False

        outcome = await EvalLoopHarness(make_agent(), NeverPass(), max_attempts=2).run(
            HarnessRequest(goal='Say "hello".')
        )
        assert outcome.passed is False


class TestHarnessLoop:
    @pytest.mark.timeout(120)
    async def test_run_streaming_emits_events(self):
        events = []
        result = ""
        passed = False
        async for evt in await HarnessLoop(
            make_agent(), make_provider(), max_attempts=3,
        ).run_streaming(HarnessRequest(
            goal="What is 9 * 9? Output only the number.",
            criteria=[__import__('deepstrike').harness.harness.Criterion(text="Answer must be exactly 81")],
        )):
            events.append(evt)
            if evt.type == "token":
                result += evt.text or ""
            if evt.type == "done":
                passed = evt.verdict.passed if evt.verdict else False
        assert isinstance(passed, bool)
        assert len(result) > 0
        assert any(e.type == "token" for e in events)
        assert any(e.type == "supervising" for e in events)
        assert any(e.type in ("done", "max_attempts_reached") for e in events)
