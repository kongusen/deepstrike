from __future__ import annotations
import pathlib
from dataclasses import dataclass, field
from typing import Any, Protocol, TYPE_CHECKING, runtime_checkable
try:
    from deepstrike._kernel import EvalPipeline
except ImportError:
    EvalPipeline = None  # type: ignore[assignment,misc]
from deepstrike.providers.stream import DoneEvent, TextDelta

if TYPE_CHECKING:
    from deepstrike.agent import Agent
    from deepstrike.providers.base import LLMProvider


@dataclass
class Criterion:
    text: str
    required: bool = True
    weight: float = 1.0


@dataclass
class CriterionResult:
    criterion: str
    passed: bool
    score: float
    feedback: str


@dataclass
class HarnessRequest:
    goal: str
    criteria: list[Criterion] | None = None
    extensions: dict[str, Any] | None = None


@dataclass
class HarnessOutcome:
    result: str
    passed: bool
    iterations: int
    total_tokens: int
    status: str
    overall_score: float | None = None
    feedback: str | None = None
    details: list[CriterionResult] = field(default_factory=list)



@runtime_checkable
class Harness(Protocol):
    async def run(self, request: HarnessRequest) -> HarnessOutcome: ...


@runtime_checkable
class QualityGate(Protocol):
    async def evaluate(self, request: HarnessRequest, outcome: HarnessOutcome) -> bool: ...


async def _run_once(agent: "Agent", goal: str, request: HarnessRequest) -> HarnessOutcome:
    done: DoneEvent | None = None
    text = ""
    async for evt in agent.run_streaming(goal, criteria=[c.text for c in (request.criteria or [])], extensions=request.extensions):
        if isinstance(evt, TextDelta):
            text += evt.delta
        elif isinstance(evt, DoneEvent):
            done = evt
    return HarnessOutcome(
        result=text,
        passed=False,
        iterations=done.iterations if done else 0,
        total_tokens=done.total_tokens if done else 0,
        status=done.status if done else "error",
    )


class SinglePassHarness:
    def __init__(self, agent: "Agent"):
        self._agent = agent

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        outcome = await _run_once(self._agent, request.goal, request)
        outcome.passed = True
        return outcome


class HarnessLoop:
    def __init__(
        self,
        agent: "Agent",
        eval_provider: "LLMProvider",
        *,
        max_attempts: int = 3,
        skill_dir: str | None = None,
    ):
        self._agent = agent
        self._eval_provider = eval_provider
        self._max_attempts = max_attempts
        self._skill_dir = pathlib.Path(skill_dir) if skill_dir else None

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        pipeline = EvalPipeline(extract_skill_on_pass=True)
        kernel_criteria = [{"text": c.text, "required": c.required, "weight": c.weight} for c in (request.criteria or [])]
        current_goal = request.goal
        outcome = HarnessOutcome(result="", passed=False, iterations=0, total_tokens=0, status="error")

        for attempt in range(1, self._max_attempts + 1):
            outcome = await _run_once(self._agent, current_goal, request)

            eval_action = pipeline.feed_outcome(request.goal, kernel_criteria, outcome.result, attempt)
            if eval_action.kind != "evaluate":
                break

            eval_text = ""
            async for evt in await self._eval_provider.stream(eval_action.messages or [], [], extensions=None):
                if isinstance(evt, TextDelta):
                    eval_text += evt.delta

            done_action = pipeline.feed_eval_result(eval_text)
            if done_action.kind != "done":
                break

            outcome.passed = done_action.passed or False
            outcome.overall_score = getattr(done_action, "overall_score", None)
            outcome.feedback = done_action.feedback
            outcome.details = [
                CriterionResult(
                    criterion=d.criterion,
                    passed=d.passed,
                    score=d.score,
                    feedback=d.feedback,
                )
                for d in (getattr(done_action, "details", None) or [])
            ]

            if outcome.passed:
                sc = done_action.skill_candidate
                if sc and self._skill_dir:
                    lines = ["---", f"name: {sc.name}", f"description: {sc.description}"]
                    if sc.when_to_use:
                        lines.append(f"when_to_use: {sc.when_to_use}")
                    lines += ["---", ""]
                    skill_path = self._skill_dir / f"{sc.name}.md"
                    skill_path.write_text("\n".join(lines) + sc.content, encoding="utf-8")
                return outcome

            current_goal = f"{request.goal}\n\n[Previous attempt {attempt} failed: {done_action.feedback}]"
            pipeline.reset()

        return outcome


class EvalLoopHarness:
    def __init__(self, agent: "Agent", gate: "QualityGate", max_attempts: int = 3):
        self._agent = agent
        self._gate = gate
        self._max_attempts = max_attempts

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        outcome = HarnessOutcome(result="", passed=False, iterations=0, total_tokens=0, status="error")
        for _ in range(self._max_attempts):
            outcome = await _run_once(self._agent, request.goal, request)
            if await self._gate.evaluate(request, outcome):
                outcome.passed = True
                return outcome
        return outcome

