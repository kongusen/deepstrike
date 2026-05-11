from __future__ import annotations
import pathlib
from dataclasses import dataclass, field
from typing import Any, TYPE_CHECKING
from deepstrike._kernel import EvalPipeline
from deepstrike.providers.stream import DoneEvent, TextDelta

if TYPE_CHECKING:
    from deepstrike.agent import Agent
    from deepstrike.providers.base import LLMProvider


@dataclass
class HarnessRequest:
    goal: str
    criteria: list[str] | None = None
    extensions: dict[str, Any] | None = None


@dataclass
class HarnessOutcome:
    result: str
    passed: bool
    iterations: int
    total_tokens: int
    status: str
    feedback: str | None = None


async def _run_once(agent: "Agent", goal: str, request: HarnessRequest) -> HarnessOutcome:
    done: DoneEvent | None = None
    text = ""
    async for evt in agent.run_streaming(goal, criteria=request.criteria, extensions=request.extensions):
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
    """
    Eval loop with LLM-as-judge and feedback injection.

    Each failed attempt feeds the evaluator's feedback back into the next goal.
    On success, if the evaluator proposes a skill candidate it is written to
    `skill_dir` for future sessions to reuse.
    """

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
        current_goal = request.goal
        outcome = HarnessOutcome(result="", passed=False, iterations=0, total_tokens=0, status="error")

        for attempt in range(1, self._max_attempts + 1):
            outcome = await _run_once(self._agent, current_goal, request)

            # Phase 1: kernel builds eval prompt
            eval_action = pipeline.feed_outcome(
                request.goal,
                request.criteria or [],
                outcome.result,
                attempt,
            )
            if eval_action.kind != "evaluate":
                break

            # Phase 2: SDK calls evaluator LLM
            eval_text = ""
            async for evt in await self._eval_provider.stream(eval_action.messages or [], [], extensions=None):
                if isinstance(evt, TextDelta):
                    eval_text += evt.delta

            # Phase 3: kernel parses verdict
            done_action = pipeline.feed_eval_result(eval_text)
            if done_action.kind != "done":
                break

            outcome.passed = done_action.passed or False
            outcome.feedback = done_action.feedback

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
