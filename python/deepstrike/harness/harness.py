"""Policy-oriented attempt orchestration.

``AttemptLoop`` owns only the loop. Running work, judging a submission, carrying
state between attempts, and stopping are four independent policy slots.
Runtime health and quality verdicts remain separate in ``AttemptOutcome``.
"""

from __future__ import annotations

import inspect
import uuid
from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Awaitable, Callable, Literal, Protocol, runtime_checkable

from deepstrike.providers.stream import DoneEvent as ProviderDoneEvent, TextDelta


@dataclass
class Criterion:
    text: str
    required: bool = True
    weight: float = 1.0
    id: str | None = None
    machine_checkable: bool | None = None


@dataclass
class CriterionResult:
    criterion: str
    passed: bool
    score: float
    feedback: str


@dataclass
class Verdict:
    passed: bool
    overall_score: float
    feedback: str
    details: list[CriterionResult] = field(default_factory=list)


@dataclass
class AttemptRequest:
    goal: str
    session_id: str | None = None
    criteria: list[Criterion] = field(default_factory=list)
    extensions: dict[str, Any] | None = None
    inherit_events: list[Any] | None = None


@dataclass
class AttemptBodyContext:
    session_id: str
    goal: str
    criteria: list[Criterion]
    extensions: dict[str, Any] | None
    inherit_events: list[Any] | None
    attempt: int
    context_input: str | None = None


@dataclass
class AttemptProgressEvent:
    type: str
    payload: dict[str, Any] = field(default_factory=dict)


@dataclass
class AttemptBodyTerminal:
    run_status: str
    result: str
    turns: int
    total_tokens: int
    submitted_nodes: list[Any] = field(default_factory=list)
    type: str = "body_done"


AttemptBodyEvent = AttemptProgressEvent | AttemptBodyTerminal


@runtime_checkable
class AttemptBody(Protocol):
    def run(self, context: AttemptBodyContext) -> AsyncIterator[AttemptBodyEvent]: ...


class RuntimeAttemptBody:
    """Adapt ``RuntimeRunner`` to the body slot without coupling the loop to run events."""

    def __init__(self, runner: Any) -> None:
        self._runner = runner

    async def run(self, context: AttemptBodyContext) -> AsyncIterator[AttemptBodyEvent]:
        if context.context_input:
            self._runner.inject_note(context.context_input)

        result = ""
        done: ProviderDoneEvent | None = None
        submitted_nodes: list[Any] = []
        try:
            events = self._runner.run(
                session_id=context.session_id,
                goal=context.goal,
                criteria=[criterion.text for criterion in context.criteria],
                extensions=context.extensions,
                inherit_events=context.inherit_events if context.attempt == 1 else None,
            )
            async for event in events:
                event_type = getattr(event, "type", None)
                if isinstance(event, TextDelta):
                    result += event.delta
                    yield AttemptProgressEvent("token", {"text": event.delta})
                elif isinstance(event, ProviderDoneEvent):
                    done = event
                elif event_type == "workflow_nodes_submitted":
                    nodes = list(getattr(event, "nodes", None) or [])
                    submitted_nodes.extend(nodes)
                    yield AttemptProgressEvent(event_type, {"nodes": nodes})
                elif event_type == "error":
                    yield AttemptProgressEvent(
                        "body_error", {"message": str(getattr(event, "message", "run failed"))}
                    )
                elif event_type in {"tool_call", "tool_delta", "tool_suspend", "tool_result"}:
                    yield AttemptProgressEvent(
                        event_type,
                        {
                            key: value
                            for key, value in vars(event).items()
                            if key != "type" and value is not None
                        },
                    )
        except Exception as error:
            yield AttemptProgressEvent("body_error", {"message": str(error)})
            yield AttemptBodyTerminal("error", result, 0, 0, submitted_nodes)
            return

        yield AttemptBodyTerminal(
            run_status=done.status if done else "error",
            result=result,
            turns=done.iterations if done else 0,
            total_tokens=done.total_tokens if done else 0,
            submitted_nodes=submitted_nodes,
        )


@dataclass
class PreparedAttempt:
    session_id: str
    goal: str
    context_input: str | None = None


@dataclass
class CarryContext:
    root_session_id: str
    goal: str
    attempt: int
    previous_verdict: Verdict | None


CarryPolicy = Callable[[CarryContext], PreparedAttempt | Awaitable[PreparedAttempt]]


def continue_session(context: CarryContext) -> PreparedAttempt:
    """Default carry: keep the transcript and inject feedback as context."""

    return PreparedAttempt(
        session_id=context.root_session_id,
        goal=context.goal,
        context_input=(
            context.previous_verdict.feedback
            if context.previous_verdict and context.previous_verdict.feedback
            else None
        ),
    )


def fresh_with_feedback(context: CarryContext) -> PreparedAttempt:
    """Explicit isolation preserving fresh-session plus goal-appended feedback semantics."""

    feedback = context.previous_verdict.feedback if context.previous_verdict else ""
    return PreparedAttempt(
        session_id=context.root_session_id if context.attempt == 1 else str(uuid.uuid4()),
        goal=(
            f"{context.goal}\n\n[Attempt {context.attempt - 1} feedback: {feedback}]"
            if feedback
            else context.goal
        ),
    )


def fresh_with_digest(
    digest: Callable[[Verdict, int], str | Awaitable[str]],
) -> CarryPolicy:
    async def prepare(context: CarryContext) -> PreparedAttempt:
        goal = context.goal
        if context.previous_verdict is not None:
            value = digest(context.previous_verdict, context.attempt - 1)
            resolved = await value if inspect.isawaitable(value) else value
            goal = f"{goal}\n\n[Prior attempt digest: {resolved}]"
        return PreparedAttempt(
            session_id=context.root_session_id if context.attempt == 1 else str(uuid.uuid4()),
            goal=goal,
        )

    return prepare


@dataclass
class StopPolicy:
    max_attempts: int
    max_total_tokens: int | None = None
    stop_on_failed_verdict: bool = False


AttemptOutcomeKind = Literal["passed", "failed_judge", "exhausted", "run_error"]


@dataclass
class AttemptOutcome:
    outcome: AttemptOutcomeKind
    run_status: str
    result: str
    attempts: int
    turns: int
    total_tokens: int
    verdict: Verdict | None = None
    submitted_nodes: list[Any] = field(default_factory=list)


@dataclass
class AttemptLoopEvent:
    type: str
    attempt: int | None = None
    verdict: Verdict | None = None
    outcome: AttemptOutcome | None = None
    progress: AttemptProgressEvent | None = None


PassHook = Callable[[AttemptOutcome, Any], None | Awaitable[None]]


class AttemptLoop:
    def __init__(
        self,
        *,
        body: AttemptBody,
        judge: Any,
        stop: StopPolicy,
        carry: CarryPolicy = continue_session,
        on_pass: PassHook | None = None,
    ) -> None:
        if stop.max_attempts < 1:
            raise ValueError("AttemptLoop stop.max_attempts must be positive")
        if stop.max_total_tokens is not None and stop.max_total_tokens < 0:
            raise ValueError("AttemptLoop stop.max_total_tokens must be non-negative")
        self._body = body
        self._judge = judge
        self._carry = carry
        self._stop = stop
        self._on_pass = on_pass

    async def run(self, request: AttemptRequest) -> AttemptOutcome:
        outcome: AttemptOutcome | None = None
        async for event in self.stream(request):
            if event.type == "completed":
                outcome = event.outcome
        if outcome is None:
            raise RuntimeError("AttemptLoop ended without an outcome")
        return outcome

    async def stream(self, request: AttemptRequest) -> AsyncIterator[AttemptLoopEvent]:
        from .judge import JudgeContext

        root_session_id = request.session_id or str(uuid.uuid4())
        previous_verdict: Verdict | None = None
        total_tokens = 0
        total_turns = 0
        submitted_nodes: list[Any] = []

        for attempt in range(1, self._stop.max_attempts + 1):
            prepared = self._carry(
                CarryContext(root_session_id, request.goal, attempt, previous_verdict)
            )
            if inspect.isawaitable(prepared):
                prepared = await prepared

            terminal: AttemptBodyTerminal | None = None
            async for event in self._body.run(
                AttemptBodyContext(
                    session_id=prepared.session_id,
                    goal=prepared.goal,
                    criteria=request.criteria,
                    extensions=request.extensions,
                    inherit_events=request.inherit_events,
                    attempt=attempt,
                    context_input=prepared.context_input,
                )
            ):
                if isinstance(event, AttemptBodyTerminal):
                    terminal = event
                    submitted_nodes.extend(event.submitted_nodes)
                else:
                    yield AttemptLoopEvent(type=event.type, progress=event)

            if terminal is None:
                raise RuntimeError("AttemptBody ended without body_done")

            total_tokens += terminal.total_tokens
            total_turns += terminal.turns
            base = dict(
                run_status=terminal.run_status,
                result=terminal.result,
                attempts=attempt,
                turns=total_turns,
                total_tokens=total_tokens,
                submitted_nodes=list(submitted_nodes),
            )

            if _is_run_error(terminal.run_status):
                outcome = AttemptOutcome(outcome="run_error", **base)
                yield AttemptLoopEvent(type="completed", outcome=outcome)
                return

            yield AttemptLoopEvent(type="judging", attempt=attempt)
            judged = await self._judge.judge(
                JudgeContext(
                    goal=request.goal,
                    criteria=request.criteria,
                    attempt=attempt,
                    result=terminal.result,
                )
            )
            if judged is None:
                raise RuntimeError("AttemptLoop judge produced no verdict")
            verdict = judged.verdict

            if verdict.passed:
                outcome = AttemptOutcome(outcome="passed", verdict=verdict, **base)
                if self._on_pass is not None:
                    hook_result = self._on_pass(outcome, judged)
                    if inspect.isawaitable(hook_result):
                        await hook_result
                yield AttemptLoopEvent(type="completed", outcome=outcome)
                return

            previous_verdict = verdict
            token_limit_reached = (
                self._stop.max_total_tokens is not None
                and total_tokens >= self._stop.max_total_tokens
            )
            if (
                self._stop.stop_on_failed_verdict
                or attempt == self._stop.max_attempts
                or token_limit_reached
            ):
                outcome = AttemptOutcome(
                    outcome=(
                        "failed_judge" if self._stop.stop_on_failed_verdict else "exhausted"
                    ),
                    verdict=verdict,
                    **base,
                )
                yield AttemptLoopEvent(type="completed", outcome=outcome)
                return

            yield AttemptLoopEvent(type="retrying", attempt=attempt, verdict=verdict)


def _is_run_error(status: str) -> bool:
    return status.lower() in {"error", "invalid_arg", "user_abort"}
