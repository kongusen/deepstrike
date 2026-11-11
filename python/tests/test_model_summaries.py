"""Plan task 8.4 · model-written compaction summaries (mirrors node/tests/model-summaries.test.ts).

A range compaction asks the runner's model for its summary through the kernel's
``summarize_context`` effect; the reply replaces the rule summary before the next provider call.
"""

import json

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, ModelSummaryOptions, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import ErrorEvent, TextDelta, ToolCallEvent, UsageEvent
from deepstrike.tools.registry import tool

SUMMARY = "MODEL SUMMARY: filled rows 1-6, 6 rows so far"


class ScriptedProvider:
    def __init__(self, fail_summary: bool = False) -> None:
        self.fail_summary = fail_summary
        self.calls = 0
        self.summary_calls = 0
        self.states: list[str] = []

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        if (context.system_stable or "").startswith("You compress"):
            self.summary_calls += 1
            if self.fail_summary:
                yield ErrorEvent(message="model unreachable")
                return
            yield TextDelta(delta=SUMMARY)
            yield UsageEvent(total_tokens=120, input_tokens=100, output_tokens=20)
            return
        self.calls += 1
        self.states.append(json.dumps(str(getattr(context.state_turn, "content", "") or "")))
        if self.calls <= 12:
            yield TextDelta(delta=f"row {self.calls} filled; {self.calls} rows so far. ")
            yield ToolCallEvent(id=f"c{self.calls}", name="fill", arguments={"n": self.calls})
            return
        yield TextDelta(delta="done")


def _runner(provider: ScriptedProvider, log: InMemorySessionLog, **extra) -> RuntimeRunner:
    @tool
    def fill(n: int) -> str:
        """Fill one row."""
        return "w" * 600

    return RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=log,
        execution_plane=LocalExecutionPlane().register(fill),
        max_tokens=1200,
        max_turns=30,
        repeat_fuse=False,
        baseline_tool_ids=["fill"],
        **extra,
    ))


async def _events(runner: RuntimeRunner, log: InMemorySessionLog, session_id: str) -> list[dict]:
    async for _ in runner.run(goal="fill rows", session_id=session_id):
        pass
    return [entry.event for entry in await log.read(session_id)]


@pytest.mark.asyncio
async def test_the_model_summary_replaces_the_rule_summary():
    provider, log = ScriptedProvider(), InMemorySessionLog()
    events = await _events(_runner(provider, log, model_summaries=True), log, "model-summary")
    summarized = [event for event in events if event.get("kind") == "context_summarized"]
    assert provider.summary_calls > 0
    assert len(summarized) == provider.summary_calls
    assert summarized[0]["status"] == "success"
    assert summarized[0]["summary"] == SUMMARY
    assert summarized[0]["usage"]["inputTokens"] == 100
    assert any("model summary" in state and SUMMARY in state for state in provider.states)
    report = next(event for event in events if event.get("kind") == "context_lifecycle_reported")
    assert report.get("model_summaries") == provider.summary_calls


@pytest.mark.asyncio
async def test_a_failed_summary_keeps_the_rule_summary():
    provider, log = ScriptedProvider(fail_summary=True), InMemorySessionLog()
    events = await _events(_runner(provider, log, model_summaries=ModelSummaryOptions(timeout_ms=5_000)),
                           log, "model-summary-failed")
    failed = next(event for event in events if event.get("kind") == "context_summarized")
    assert failed["status"] == "failed"
    assert "model unreachable" in failed["error"]
    assert any("[Compressed:" in state for state in provider.states)
    assert not any("model summary" in state for state in provider.states)


@pytest.mark.asyncio
async def test_no_summary_is_asked_for_unless_enabled():
    provider, log = ScriptedProvider(), InMemorySessionLog()
    events = await _events(_runner(provider, log), log, "model-summary-off")
    assert any(event.get("kind") == "compressed" for event in events)
    assert provider.summary_calls == 0
