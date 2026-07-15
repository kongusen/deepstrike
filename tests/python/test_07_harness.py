"""07 — AttemptLoop body/judge/carry/stop contract."""

import pytest

from deepstrike import (
    AttemptLoop,
    AttemptRequest,
    Criterion,
    LlmEvalJudge,
    RuntimeAttemptBody,
    StopPolicy,
    Verdict,
    VerdictFnJudge,
)

from conftest import make_agent, make_provider


class TestAttemptLoop:
    @pytest.mark.timeout(60)
    async def test_host_judge_passes_and_returns_real_result(self):
        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=VerdictFnJudge(lambda **_: Verdict(True, 1.0, "ok")),
            stop=StopPolicy(max_attempts=1),
        )
        outcome = await loop.run(AttemptRequest(goal='Reply "done".'))
        assert outcome.outcome == "passed"
        assert len(outcome.result) > 0
        assert outcome.turns >= 0
        assert outcome.total_tokens >= 0

    @pytest.mark.timeout(120)
    async def test_retries_then_passes(self):
        attempts = 0

        def verdict(**_):
            nonlocal attempts
            attempts += 1
            return Verdict(attempts >= 2, 1.0 if attempts >= 2 else 0.0, "retry")

        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=VerdictFnJudge(verdict),
            stop=StopPolicy(max_attempts=3),
        )
        outcome = await loop.run(AttemptRequest(session_id="stable", goal='Say "hello".'))
        assert outcome.outcome == "passed"
        assert outcome.attempts == 2

    @pytest.mark.timeout(120)
    async def test_exhaustion_is_not_a_body_run_error(self):
        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=VerdictFnJudge(lambda **_: Verdict(False, 0.0, "no")),
            stop=StopPolicy(max_attempts=2),
        )
        outcome = await loop.run(AttemptRequest(goal='Say "hello".'))
        assert outcome.outcome == "exhausted"
        assert outcome.run_status != "error"
        assert outcome.verdict and outcome.verdict.passed is False

    @pytest.mark.timeout(120)
    async def test_stream_emits_progress_judging_and_terminal(self):
        events = []
        result = ""
        loop = AttemptLoop(
            body=RuntimeAttemptBody(make_agent()._runner),
            judge=LlmEvalJudge(make_provider()),
            stop=StopPolicy(max_attempts=2),
        )
        async for event in loop.stream(AttemptRequest(
            goal="What is 9 * 9? Output only the number.",
            criteria=[Criterion(text="Answer must be exactly 81")],
        )):
            events.append(event)
            if event.type == "token" and event.progress:
                result += str(event.progress.payload.get("text", ""))
        assert len(result) > 0
        assert any(event.type == "judging" for event in events)
        assert any(event.type == "completed" for event in events)
