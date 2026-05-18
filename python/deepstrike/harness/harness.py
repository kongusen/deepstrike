from __future__ import annotations
import pathlib
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Protocol, TYPE_CHECKING, Union, runtime_checkable
from deepstrike._kernel import EvalPipeline
from deepstrike.providers.stream import DoneEvent as _ProviderDoneEvent, TextDelta

from deepstrike.providers.base import RenderedContext
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


@dataclass
class Verdict:
    passed: bool
    overall_score: float
    feedback: str
    details: list[CriterionResult]


@dataclass
class TokenEvent:
    text: str
    type: str = "token"


@dataclass
class ToolCallEvent:
    id: str | None = None
    name: str | None = None
    type: str = "tool_call"


@dataclass
class ToolDeltaEvent:
    call_id: str
    delta: str
    type: str = "tool_delta"


@dataclass
class ToolSuspendEvent:
    call_id: str
    suspension_id: str
    payload: dict | None = None
    type: str = "tool_suspend"


@dataclass
class ToolResultEvent:
    call_id: str | None = None
    content: str | None = None
    is_error: bool | None = None
    type: str = "tool_result"


@dataclass
class SupervisingEvent:
    type: str = "supervising"


@dataclass
class RevisingEvent:
    verdict: Verdict
    type: str = "revising"


@dataclass
class DoneEvent:
    verdict: Verdict
    iterations: int
    total_tokens: int
    status: str
    type: str = "done"


@dataclass
class MaxAttemptsReachedEvent:
    type: str = "max_attempts_reached"


HarnessEvent = Union[
    TokenEvent, ToolCallEvent, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent,
    SupervisingEvent, RevisingEvent, DoneEvent, MaxAttemptsReachedEvent,
]


@runtime_checkable
class QualityGate(Protocol):
    async def evaluate(self, request: HarnessRequest, outcome: HarnessOutcome) -> bool: ...


async def _run_once(agent: "Agent", goal: str, request: HarnessRequest) -> HarnessOutcome:
    done: _ProviderDoneEvent | None = None
    text = ""
    async for evt in agent.run_streaming(goal, criteria=[c.text for c in (request.criteria or [])], extensions=request.extensions):
        if isinstance(evt, TextDelta):
            text += evt.delta
        elif isinstance(evt, _ProviderDoneEvent):
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

    def run_streaming(self, request: HarnessRequest) -> AsyncIterator[HarnessEvent]:
        return self._run_streaming_impl(request)

    async def _run_streaming_impl(self, request: HarnessRequest) -> AsyncIterator[HarnessEvent]:
        pipeline = EvalPipeline(extract_skill_on_pass=True)
        criteria = request.criteria or []

        current_goal = request.goal
        last_iterations = 0
        last_total_tokens = 0
        last_status = "error"
        last_result = ""

        for attempt in range(1, self._max_attempts + 1):
            async for evt in self._agent.run_streaming(current_goal, criteria=[c.text for c in criteria], extensions=request.extensions):
                if isinstance(evt, TextDelta):
                    last_result += evt.delta
                    yield TokenEvent(text=evt.delta)
                elif isinstance(evt, _ProviderDoneEvent):
                    last_iterations = evt.iterations
                    last_total_tokens = evt.total_tokens
                    last_status = evt.status
                else:
                    kind = getattr(evt, "type", None)
                    if kind == "tool_call":
                        yield ToolCallEvent(id=getattr(evt, "id", None), name=getattr(evt, "name", None))
                    elif kind == "tool_delta":
                        yield ToolDeltaEvent(call_id=getattr(evt, "call_id", None), delta=getattr(evt, "delta", None))
                    elif kind == "tool_suspend":
                        yield ToolSuspendEvent(call_id=getattr(evt, "call_id", None), suspension_id=getattr(evt, "suspension_id", None), payload=getattr(evt, "payload", None))
                    elif kind == "tool_result":
                        yield ToolResultEvent(call_id=getattr(evt, "call_id", None), content=getattr(evt, "content", None), is_error=getattr(evt, "is_error", None))

            yield SupervisingEvent()

            eval_action = pipeline.feed_outcome(request.goal, criteria, last_result, attempt)
            if eval_action.kind != "evaluate":
                break

            eval_text = ""
            eval_msgs = eval_action.messages or []
            eval_system = "\n\n".join(m.content for m in eval_msgs if m.role == "system")
            eval_turns = [m for m in eval_msgs if m.role != "system"]
            eval_context = RenderedContext(system_text=eval_system, turns=eval_turns)
            async for evt in self._eval_provider.stream(eval_context, [], extensions=None):
                if isinstance(evt, TextDelta):
                    eval_text += evt.delta

            done_action = pipeline.feed_eval_result(eval_text)
            if done_action.kind != "done":
                break

            verdict = Verdict(
                passed=done_action.passed or False,
                overall_score=getattr(done_action, "overall_score", 0.0) or 0.0,
                feedback=done_action.feedback or "",
                details=[
                    CriterionResult(criterion=d.criterion, passed=d.passed, score=d.score, feedback=d.feedback)
                    for d in (getattr(done_action, "details", None) or [])
                ],
            )

            if verdict.passed:
                sc = done_action.skill_candidate
                if sc and self._skill_dir:
                    lines = ["---", f"name: {sc.name}", f"description: {sc.description}"]
                    if sc.when_to_use:
                        lines.append(f"when_to_use: {sc.when_to_use}")
                    lines += ["---", ""]
                    skill_path = self._skill_dir / f"{sc.name}.md"
                    skill_path.write_text("\n".join(lines) + sc.content, encoding="utf-8")
                yield DoneEvent(verdict=verdict, iterations=last_iterations, total_tokens=last_total_tokens, status=last_status)
                return

            yield RevisingEvent(verdict=verdict)
            current_goal = f"{request.goal}\n\n[Attempt {attempt} feedback: {verdict.feedback}]"
            last_result = ""
            pipeline.reset()

        yield MaxAttemptsReachedEvent()
