from __future__ import annotations
import pathlib
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Protocol, TYPE_CHECKING, Union, runtime_checkable
from deepstrike._kernel import build_eval_messages, parse_verdict
from deepstrike.providers.stream import DoneEvent as _ProviderDoneEvent, TextDelta

from deepstrike.providers.base import RenderedContext
if TYPE_CHECKING:
    from deepstrike.runtime import RuntimeRunner
    from deepstrike.providers.base import LLMProvider


@dataclass
class Criterion:
    text: str
    required: bool = True
    weight: float = 1.0
    # I3.3 (A4): optional stable id from the host's contract layer (threaded to verdict_fn).
    id: "str | None" = None
    # I3.3 (A4): host hint — host has a deterministic check for this criterion.
    machine_checkable: "bool | None" = None


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
    # R3-1: nodes the agent submitted via `submit_workflow_nodes` while running under the harness.
    submitted_nodes: list = field(default_factory=list)


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
    is_fatal: bool | None = None
    error_kind: str | None = None
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


async def _run_once(runner: "RuntimeRunner", goal: str, request: HarnessRequest) -> HarnessOutcome:
    done: _ProviderDoneEvent | None = None
    text = ""
    async for evt in runner.run(goal=goal, criteria=[c.text for c in (request.criteria or [])], extensions=request.extensions):
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
    def __init__(self, runner: "RuntimeRunner"):
        self._runner = runner

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        outcome = await _run_once(self._runner, request.goal, request)
        outcome.passed = True
        return outcome

    async def stream(self, request: HarnessRequest) -> "AsyncIterator[StreamEvent]":
        return self._runner.run(goal=request.goal, criteria=[c.text for c in (request.criteria or [])], extensions=request.extensions)


class EvalLoopHarness:
    """I3.4 (A1): deprecated — prefer ``HarnessLoop`` with ``verdict_fn`` for host-defined
    judgment. ``EvalLoopHarness.stream()`` does NOT honor ``gate`` (only ``.run()`` does);
    ``HarnessLoop`` runs the eval loop uniformly across stream and run."""
    def __init__(self, runner: "RuntimeRunner", gate: "QualityGate", max_attempts: int = 3):
        self._runner = runner
        self._gate = gate
        self._max_attempts = max_attempts

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        outcome = HarnessOutcome(result="", passed=False, iterations=0, total_tokens=0, status="error")
        for _ in range(self._max_attempts):
            outcome = await _run_once(self._runner, request.goal, request)
            if await self._gate.evaluate(request, outcome):
                outcome.passed = True
                return outcome
        return outcome

    async def stream(self, request: HarnessRequest) -> "AsyncIterator[StreamEvent]":
        return self._runner.run(goal=request.goal, criteria=[c.text for c in (request.criteria or [])], extensions=request.extensions)


class HarnessLoop:
    def __init__(
        self,
        runner: "RuntimeRunner",
        eval_provider: "LLMProvider",
        *,
        max_attempts: int = 3,
        skill_dir: str | None = None,
        # I3.2 (A2/A3): host-supplied judgment. Receives kwargs (goal, criteria, attempt, result);
        # returns Verdict to short-circuit the LLM eval, or None/Awaitable[None] to defer. Mirrors
        # the Node SDK ``verdictFn``.
        verdict_fn=None,
    ):
        self._runner = runner
        self._eval_provider = eval_provider
        self._max_attempts = max_attempts
        self._skill_dir = pathlib.Path(skill_dir) if skill_dir else None
        self._verdict_fn = verdict_fn

    async def run(self, request: HarnessRequest) -> HarnessOutcome:
        # R3-1: collect nodes the agent submits while running under the harness (dynamic fan-out in
        # harness mode too, not just the plain streaming path).
        self._submitted_nodes: list = []
        last: "HarnessEvent | None" = None
        async for evt in self.stream(request):
            last = evt
        done = last if last and getattr(last, "type", None) == "done" else None
        return HarnessOutcome(
            result="",
            passed=done.verdict.passed if done else False,
            iterations=done.iterations if done else 0,
            total_tokens=done.total_tokens if done else 0,
            status=done.status if done else "error",
            overall_score=done.verdict.overall_score if done else None,
            feedback=done.verdict.feedback if done else None,
            details=done.verdict.details if done else None,
            submitted_nodes=list(self._submitted_nodes),
        )

    def stream(self, request: HarnessRequest) -> "AsyncIterator[HarnessEvent]":
        return self._run_streaming_impl(request)

    async def _run_streaming_impl(self, request: HarnessRequest) -> AsyncIterator[HarnessEvent]:
        criteria = request.criteria or []

        current_goal = request.goal
        last_iterations = 0
        last_total_tokens = 0
        last_status = "error"
        last_result = ""

        for attempt in range(1, self._max_attempts + 1):
            async for evt in self._runner.run(goal=current_goal, criteria=[c.text for c in criteria], extensions=request.extensions):
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
                    elif kind == "workflow_nodes_submitted":
                        # R3-1: accumulate submitted nodes for run() to surface on the outcome.
                        if not hasattr(self, "_submitted_nodes"):
                            self._submitted_nodes = []
                        self._submitted_nodes.extend(getattr(evt, "nodes", None) or [])

            yield SupervisingEvent()

            # I3.2 (A2/A3): host-supplied verdict_fn short-circuits the LLM eval. Returning None
            # (or awaitable None) defers to the built-in eval below — enables hybrid judgment.
            verdict = None
            skill_candidate = None
            if self._verdict_fn is not None:
                result = self._verdict_fn(goal=request.goal, criteria=criteria, attempt=attempt, result=last_result)
                if hasattr(result, "__await__"):
                    result = await result
                verdict = result
            if verdict is None:
                # #6 (0.5.0): the eval/verdict compute is the kernel's stateless free functions (was the
                # EvalPipeline state machine). Build the eval prompt, call the eval LLM, parse the verdict.
                eval_msgs = build_eval_messages(request.goal, criteria, last_result, attempt, True)
                eval_text = ""
                eval_system = "\n\n".join(m.content for m in eval_msgs if m.role == "system")
                eval_turns = [m for m in eval_msgs if m.role != "system"]
                eval_context = RenderedContext(system_text=eval_system, turns=eval_turns)
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
                skill_candidate = parsed.skill_candidate

            if verdict.passed:
                sc = skill_candidate
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

        yield MaxAttemptsReachedEvent()
