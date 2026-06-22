"""P4 — AttemptJudge Strategy (Python parity with Node's harness-judge / harness-verdictfn).

Two layers:
  • Isolated strategy units (VerdictFnJudge / LlmEvalJudge / HybridJudge), mirroring
    node/tests/harness-judge.test.ts.
  • A HarnessLoop verdict_fn behavior-lock (short-circuit / defer-to-LLM / feedback threading),
    which Python lacked before this refactor — proves the extraction is behavior-preserving.
"""
from __future__ import annotations

import json

import pytest

from deepstrike.harness.harness import Criterion, HarnessLoop, HarnessRequest, Verdict
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


# ── HarnessLoop behavior-lock (verdict_fn wiring) ───────────────────────────

class _Runner:
    """Minimal RuntimeRunner stand-in: emits the agent text then a DoneEvent."""

    def __init__(self, text: str = "agent output"):
        self._text = text

    async def run(self, *, goal, criteria, extensions=None):
        yield TextDelta(delta=self._text)
        yield DoneEvent(iterations=1, total_tokens=42, status="completed")


async def _collect(loop, request):
    return [evt async for evt in loop.stream(request)]


@pytest.mark.asyncio
async def test_harness_verdictfn_short_circuits_llm():
    """A host verdict_fn returning a passing Verdict ends the loop without touching the LLM eval."""
    provider = _EvalProvider(PASS_JSON)
    fn = lambda **_: Verdict(passed=True, overall_score=1.0, feedback="host-pass", details=[])
    loop = HarnessLoop(_Runner(), provider, verdict_fn=fn, max_attempts=3)
    outcome = await loop.run(HarnessRequest(goal="g", criteria=[Criterion(text="c1")]))
    assert outcome.passed is True
    assert outcome.feedback == "host-pass"
    assert provider.calls == 0  # LLM eval short-circuited


@pytest.mark.asyncio
async def test_harness_verdictfn_defers_to_llm():
    """verdict_fn returning None defers to the built-in LLM eval."""
    provider = _EvalProvider(PASS_JSON)
    loop = HarnessLoop(_Runner(), provider, verdict_fn=lambda **_: None, max_attempts=3)
    outcome = await loop.run(HarnessRequest(goal="g", criteria=[Criterion(text="c1")]))
    assert outcome.passed is True
    assert provider.calls == 1  # fell through to the LLM eval


@pytest.mark.asyncio
async def test_harness_verdictfn_feedback_threaded_into_retry_goal():
    """On a failing verdict the feedback is threaded into the next attempt's goal, then it passes."""
    seen_goals: list[str] = []

    class _RecordingRunner(_Runner):
        async def run(self, *, goal, criteria, extensions=None):
            seen_goals.append(goal)
            async for evt in super().run(goal=goal, criteria=criteria, extensions=extensions):
                yield evt

    verdicts = iter([
        Verdict(passed=False, overall_score=0.0, feedback="fix-this", details=[]),
        Verdict(passed=True, overall_score=1.0, feedback="ok", details=[]),
    ])
    loop = HarnessLoop(_RecordingRunner(), _EvalProvider(PASS_JSON), verdict_fn=lambda **_: next(verdicts), max_attempts=3)
    outcome = await loop.run(HarnessRequest(goal="base-goal", criteria=[Criterion(text="c1")]))
    assert outcome.passed is True
    assert len(seen_goals) == 2
    assert seen_goals[0] == "base-goal"
    assert "fix-this" in seen_goals[1]  # feedback threaded into the retry goal
