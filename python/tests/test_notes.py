"""Plan task 9.1 · notes the model writes with the kernel's ``note`` tool (mirrors
node/tests/notes.test.ts): offered only with a notes budget, consumed by the kernel, rendered in
the task state from the next turn on, and counted in the lifecycle report."""

import json

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


class ScriptedProvider:
    def __init__(self) -> None:
        self.calls = 0
        self.offered: list[list[str]] = []
        self.states: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.calls += 1
        self.offered.append([t.name for t in tools])
        self.states.append(json.dumps(str(getattr(context.state_turn, "content", "") or "")))
        if self.calls == 1:
            yield ToolCallEvent(id="c1", name="read", arguments={"path": "logs/a.log"})
            return
        if self.calls == 2 and "note" in self.offered[-1]:
            yield ToolCallEvent(id="c2", name="note", arguments={
                "title": "Tally", "kind": "state", "body": "3 qualifying lines in logs/a.log",
            })
            return
        yield TextDelta(delta="3")


def _runner(provider: ScriptedProvider, log: InMemorySessionLog, policy: dict | None) -> RuntimeRunner:
    @tool
    def read(path: str) -> str:
        """Read a file."""
        return "ERROR x\nERROR y\nERROR z"

    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=log,
        execution_plane=LocalExecutionPlane().register(read),
        max_tokens=8000,
        max_turns=10,
        baseline_tool_ids=["read"],
        context_policy=policy,
    ))


async def _events(runner: RuntimeRunner, log: InMemorySessionLog, session_id: str) -> list[dict]:
    async for _ in runner.run(goal="count the errors", session_id=session_id):
        pass
    return [entry.event for entry in await log.read(session_id)]


@pytest.mark.asyncio
async def test_notes_are_written_by_the_model_kept_by_the_kernel_and_shown_in_the_task_state() -> None:
    provider, log = ScriptedProvider(), InMemorySessionLog()
    events = await _events(_runner(provider, log, {"notes_budget_tokens": 600}), log, "notes")
    assert "note" in provider.offered[0]
    assert "[state] Tally: 3 qualifying lines in logs/a.log" in provider.states[2]
    report = [e for e in events if e.get("kind") == "context_lifecycle_reported"][-1]
    assert report["notes_written"] == 1
    assert report["notes_active"] == 1
    assert any(edge["relation"] == "derived_from" for edge in report.get("edges", []))


@pytest.mark.asyncio
async def test_notes_are_off_without_a_budget() -> None:
    provider, log = ScriptedProvider(), InMemorySessionLog()
    await _events(_runner(provider, log, None), log, "no-notes")
    assert "note" not in provider.offered[0]


def test_a_negative_budget_is_rejected() -> None:
    from deepstrike.runtime.context_policy import context_policy

    with pytest.raises(ValueError, match="notes_budget_tokens"):
        context_policy({"notes_budget_tokens": -1})
