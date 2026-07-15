"""AttemptJudge strategies and their composition with AttemptLoop."""
from __future__ import annotations

import json

import pytest

from deepstrike.harness.harness import (
    AttemptLoop,
    AttemptRequest,
    Criterion,
    RuntimeAttemptBody,
    StopPolicy,
    Verdict,
)
from deepstrike.harness.judge import HybridJudge, JudgeContext, LlmEvalJudge, VerdictFnJudge
from deepstrike.providers.stream import DoneEvent, TextDelta


CTX = JudgeContext(goal="g", criteria=[Criterion(text="c1", required=True)], attempt=1, result="out")
PASS_JSON = json.dumps({"passed": True, "overall_score": 1, "feedback": "ok", "details": []})


class _EvalProvider:
    """Streams a fixed verdict JSON; counts how often it is invoked."""

    def __init__(self, verdict_json: str):
        self._json = verdict_json
        self.calls = 0

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        yield TextDelta(delta=self._json)
        yield DoneEvent(iterations=1, total_tokens=10, status="completed")


# ── Isolated strategy units ─────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_verdictfn_judge_returns_verdict():
    fn = lambda **_: Verdict(passed=False, overall_score=0, feedback="nope", details=[])
    res = await VerdictFnJudge(fn).judge(CTX)
    assert res is not None
    assert res.verdict.passed is False
    assert res.verdict.feedback == "nope"


@pytest.mark.asyncio
async def test_verdictfn_judge_defers_on_none():
    res = await VerdictFnJudge(lambda **_: None).judge(CTX)
    assert res is None


@pytest.mark.asyncio
async def test_verdictfn_judge_awaits_coroutine_result():
    async def fn(**_):
        return Verdict(passed=True, overall_score=1, feedback="async", details=[])
    res = await VerdictFnJudge(fn).judge(CTX)
    assert res is not None and res.verdict.feedback == "async"


@pytest.mark.asyncio
async def test_llm_eval_judge_streams_and_parses():
    provider = _EvalProvider(PASS_JSON)
    res = await LlmEvalJudge(provider).judge(CTX)
    assert provider.calls == 1
    assert res.verdict.passed is True


@pytest.mark.asyncio
async def test_hybrid_uses_primary_skips_fallback():
    provider = _EvalProvider(PASS_JSON)
    fn = lambda **_: Verdict(passed=True, overall_score=1, feedback="host", details=[])
    res = await HybridJudge(VerdictFnJudge(fn), LlmEvalJudge(provider)).judge(CTX)
    assert res is not None and res.verdict.feedback == "host"
    assert provider.calls == 0  # fallback never invoked


@pytest.mark.asyncio
async def test_hybrid_falls_back_when_primary_defers():
    provider = _EvalProvider(PASS_JSON)
    res = await HybridJudge(VerdictFnJudge(lambda **_: None), LlmEvalJudge(provider)).judge(CTX)
    assert provider.calls == 1
    assert res is not None and res.verdict.passed is True


# ── AttemptLoop composition ─────────────────────────────────────────────────

class _Runner:
    """Minimal RuntimeRunner stand-in: emits the agent text then a DoneEvent."""

    def __init__(self, text: str = "agent output"):
        self._text = text
        self.notes: list[str] = []
        self.sessions: list[str] = []

    def inject_note(self, text: str) -> None:
        self.notes.append(text)

    async def run(
        self, *, goal, criteria, extensions=None, session_id=None, inherit_events=None
    ):
        self.sessions.append(session_id)
        yield TextDelta(delta=self._text)
        yield DoneEvent(iterations=1, total_tokens=42, status="completed")


async def _collect(loop, request):
    return [evt async for evt in loop.stream(request)]


@pytest.mark.asyncio
async def test_attempt_loop_verdictfn_short_circuits_llm():
    """A host verdict_fn returning a passing Verdict ends the loop without touching the LLM eval."""
    provider = _EvalProvider(PASS_JSON)
    fn = lambda **_: Verdict(passed=True, overall_score=1.0, feedback="host-pass", details=[])
    loop = AttemptLoop(
        body=RuntimeAttemptBody(_Runner()),
        judge=HybridJudge(VerdictFnJudge(fn), LlmEvalJudge(provider)),
        stop=StopPolicy(max_attempts=3),
    )
    outcome = await loop.run(AttemptRequest(goal="g", criteria=[Criterion(text="c1")]))
    assert outcome.outcome == "passed"
    assert outcome.verdict and outcome.verdict.feedback == "host-pass"
    assert provider.calls == 0  # LLM eval short-circuited


@pytest.mark.asyncio
async def test_attempt_loop_verdictfn_defers_to_llm():
    """verdict_fn returning None defers to the built-in LLM eval."""
    provider = _EvalProvider(PASS_JSON)
    loop = AttemptLoop(
        body=RuntimeAttemptBody(_Runner()),
        judge=HybridJudge(VerdictFnJudge(lambda **_: None), LlmEvalJudge(provider)),
        stop=StopPolicy(max_attempts=3),
    )
    outcome = await loop.run(AttemptRequest(goal="g", criteria=[Criterion(text="c1")]))
    assert outcome.outcome == "passed"
    assert provider.calls == 1  # fell through to the LLM eval


@pytest.mark.asyncio
async def test_attempt_loop_default_carry_keeps_session_and_injects_feedback():
    """The default carry preserves both the stable goal and stable session transcript."""
    seen_goals: list[str] = []

    class _RecordingRunner(_Runner):
        async def run(
            self, *, goal, criteria, extensions=None, session_id=None, inherit_events=None
        ):
            seen_goals.append(goal)
            async for evt in super().run(
                goal=goal,
                criteria=criteria,
                extensions=extensions,
                session_id=session_id,
                inherit_events=inherit_events,
            ):
                yield evt

    verdicts = iter([
        Verdict(passed=False, overall_score=0.0, feedback="fix-this", details=[]),
        Verdict(passed=True, overall_score=1.0, feedback="ok", details=[]),
    ])
    runner = _RecordingRunner()
    loop = AttemptLoop(
        body=RuntimeAttemptBody(runner),
        judge=VerdictFnJudge(lambda **_: next(verdicts)),
        stop=StopPolicy(max_attempts=3),
    )
    outcome = await loop.run(
        AttemptRequest(session_id="stable", goal="base-goal", criteria=[Criterion(text="c1")])
    )
    assert outcome.outcome == "passed"
    assert seen_goals == ["base-goal", "base-goal"]
    assert runner.sessions == ["stable", "stable"]
    assert runner.notes == ["fix-this"]


@pytest.mark.asyncio
async def test_attempt_loop_run_error_skips_judge():
    class _ErrorRunner(_Runner):
        async def run(self, **kwargs):
            yield TextDelta(delta="partial")
            yield DoneEvent(iterations=1, total_tokens=7, status="error")

    calls = 0

    def verdict(**_):
        nonlocal calls
        calls += 1
        return Verdict(passed=True, overall_score=1, feedback="wrong")

    outcome = await AttemptLoop(
        body=RuntimeAttemptBody(_ErrorRunner()),
        judge=VerdictFnJudge(verdict),
        stop=StopPolicy(max_attempts=2),
    ).run(AttemptRequest(goal="g"))
    assert outcome.outcome == "run_error"
    assert outcome.verdict is None
    assert outcome.result == "partial"
    assert calls == 0
