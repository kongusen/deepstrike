"""AttemptJudge — the independent "how do we judge one attempt?" policy slot.

The built-in strategies compose without changing the attempt engine:
  • ``VerdictFnJudge`` — host-supplied deterministic judgment; returns ``None`` to defer (and awaits a
    coroutine result, mirroring the inline ``hasattr(result, "__await__")`` handling).
  • ``LlmEvalJudge``  — the kernel's stateless eval (``build_eval_messages`` → stream → ``parse_verdict``).
  • ``HybridJudge``   — try the primary; on ``None``, fall back (verdict_fn → LLM eval).
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Protocol, runtime_checkable

from deepstrike._kernel import build_eval_messages, parse_verdict
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta

from .harness import Criterion, CriterionResult, Verdict


@dataclass
class JudgeContext:
    goal: str
    criteria: list[Criterion]
    attempt: int
    result: str


@dataclass
class JudgeResult:
    verdict: Verdict
    # Skill the judge proposes extracting on pass (LLM-eval path only).
    skill_candidate: Any = None


@runtime_checkable
class AttemptJudge(Protocol):
    """Decides whether one attempt's result meets the criteria. Returning ``None`` defers to a
    fallback judge (enables hybrid host/LLM judgment)."""

    async def judge(self, ctx: JudgeContext) -> JudgeResult | None: ...


class VerdictFnJudge:
    """Wraps a host-supplied verdict function. Returns ``None`` (defer) when the function does;
    awaits the result when the function is a coroutine (parity with the prior inline handling)."""

    def __init__(self, fn):
        self._fn = fn

    async def judge(self, ctx: JudgeContext) -> JudgeResult | None:
        result = self._fn(goal=ctx.goal, criteria=ctx.criteria, attempt=ctx.attempt, result=ctx.result)
        if hasattr(result, "__await__"):
            result = await result
        return JudgeResult(verdict=result) if result is not None else None


class LlmEvalJudge:
    """The built-in LLM eval: render the kernel eval prompt, stream the eval provider, parse the
    verdict. Always produces a ``JudgeResult`` (never defers)."""

    def __init__(self, eval_provider, extract_skill_on_pass: bool = False):
        self._eval_provider = eval_provider
        self._extract_skill_on_pass = extract_skill_on_pass

    async def judge(self, ctx: JudgeContext) -> JudgeResult:
        eval_msgs = build_eval_messages(ctx.goal, ctx.criteria, ctx.result, ctx.attempt, self._extract_skill_on_pass)
        eval_system = "\n\n".join(m.content for m in eval_msgs if m.role == "system")
        eval_turns = [m for m in eval_msgs if m.role != "system"]
        eval_context = RenderedContext(system_text=eval_system, turns=eval_turns)
        eval_text = ""
        async for evt in self._eval_provider.stream(eval_context, [], extensions=None):
            if isinstance(evt, TextDelta):
                eval_text += evt.delta
        parsed = parse_verdict(eval_text)
        verdict = Verdict(
            passed=parsed.passed,
            overall_score=parsed.overall_score,
            feedback=parsed.feedback,
            details=[
                CriterionResult(criterion=d.criterion, passed=d.passed, score=d.score, feedback=d.feedback)
                for d in (parsed.details or [])
            ],
        )
        return JudgeResult(verdict=verdict, skill_candidate=parsed.skill_candidate)


class HybridJudge:
    """Try ``primary``; if it defers (``None``), use ``fallback``."""

    def __init__(self, primary: AttemptJudge, fallback: AttemptJudge):
        self._primary = primary
        self._fallback = fallback

    async def judge(self, ctx: JudgeContext) -> JudgeResult | None:
        return await self._primary.judge(ctx) or await self._fallback.judge(ctx)
