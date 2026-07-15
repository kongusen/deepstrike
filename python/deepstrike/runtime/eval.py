"""`judge()` — one-shot quality scoring against a goal + criteria.

Python port of node/src/runtime/eval.ts. Wraps the kernel's `gen_eval` free functions
(`build_eval_messages` / `parse_verdict` / `verdict_output_schema`) into a small typed
surface so callers can score one (goal, criteria, result) pair without standing up
an ``AttemptLoop``.

Single LLM call: build the eval prompt → stream → parse verdict.
"""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from deepstrike._kernel import (  # type: ignore
    Message,
    build_eval_messages as _kernel_build_eval_messages,
    parse_verdict as _kernel_parse_verdict,
    verdict_output_schema as _kernel_verdict_output_schema,
)
from deepstrike.providers.base import LLMProvider, RenderedContext  # type: ignore


@dataclass
class Criterion:
    text: str
    required: bool = True
    weight: float | None = None


@dataclass
class VerdictDetail:
    criterion: str
    passed: bool
    score: float
    feedback: str


@dataclass
class Verdict:
    passed: bool
    overall_score: float
    feedback: str
    details: list[VerdictDetail] = field(default_factory=list)


def build_eval_messages(goal: str, criteria: list[Criterion], result: str) -> list[Message]:
    """Render the kernel's eval prompt for (goal, criteria, result)."""
    native_criteria = [
        {"text": c.text, "required": c.required, **({"weight": c.weight} if c.weight is not None else {})}
        for c in criteria
    ]
    return _kernel_build_eval_messages(goal, native_criteria, result, 1, False)


def parse_verdict(text: str) -> Verdict:
    """Parse a Verdict from raw judge-LLM text."""
    raw = _kernel_parse_verdict(text)
    details_raw = raw.get("details") if isinstance(raw, dict) else getattr(raw, "details", None)
    details: list[VerdictDetail] = []
    for d in details_raw or []:
        if isinstance(d, dict):
            details.append(VerdictDetail(
                criterion=d.get("criterion", ""),
                passed=bool(d.get("passed", False)),
                score=float(d.get("score", 0.0)),
                feedback=d.get("feedback", ""),
            ))
    if isinstance(raw, dict):
        return Verdict(
            passed=bool(raw.get("passed", False)),
            overall_score=float(raw.get("overallScore", raw.get("overall_score", 0.0))),
            feedback=raw.get("feedback", ""),
            details=details,
        )
    return Verdict(
        passed=bool(getattr(raw, "passed", False)),
        overall_score=float(getattr(raw, "overall_score", 0.0)),
        feedback=getattr(raw, "feedback", ""),
        details=details,
    )


def verdict_output_schema() -> dict[str, Any]:
    """The JSON Schema the kernel expects judge output to conform to."""
    return json.loads(_kernel_verdict_output_schema(False))


async def judge(
    provider: LLMProvider,
    goal: str,
    criteria: list[Criterion],
    result: str,
) -> Verdict:
    """Run one judge pass: render eval prompt, stream the provider, parse the verdict."""
    msgs = build_eval_messages(goal, criteria, result)
    sys_text = "\n\n".join(m.get("content", "") if isinstance(m, dict) else getattr(m, "content", "")
                           for m in msgs
                           if (m.get("role") if isinstance(m, dict) else getattr(m, "role", None)) == "system")
    turns = [m for m in msgs if (m.get("role") if isinstance(m, dict) else getattr(m, "role", None)) != "system"]
    ctx: RenderedContext = {"systemText": sys_text, "turns": turns}  # type: ignore[assignment]

    text = ""
    async for evt in provider.stream(ctx, [], None):
        if (evt.get("type") if isinstance(evt, dict) else getattr(evt, "type", None)) == "text_delta":
            text += (evt.get("delta") if isinstance(evt, dict) else getattr(evt, "delta", "")) or ""
    if not text:
        raise RuntimeError("judge: provider produced no text")
    return parse_verdict(text)
