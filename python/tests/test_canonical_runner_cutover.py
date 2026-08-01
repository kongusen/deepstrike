from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from deepstrike._kernel import Message
from deepstrike.kernel.canonical import CanonicalKernel
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime
from deepstrike.runtime.execution_plane import LocalExecutionPlane
from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.runtime.session_log import InMemorySessionLog
from deepstrike.tools.registry import tool
from deepstrike.types.agent import LoopResult, SubAgentResult


class _LiveJournal:
    async def head(self, operation_id: str):
        return object()


class _LiveCanonicalRuntime:
    operation_id = "python-operation-run-1"
    journal = _LiveJournal()

    def __init__(self) -> None:
        self.restored = False

    async def restore(self) -> None:
        self.restored = True

    def is_terminal(self) -> bool:
        return False


async def _empty_execute(*_args, **_kwargs):
    if False:
        yield None


@pytest.mark.asyncio
async def test_forged_run_terminal_does_not_suppress_wake(monkeypatch):
    log = InMemorySessionLog()
    await log.append("session", {"kind": "run_started", "run_id": "run-1", "goal": "goal", "criteria": []})
    await log.append("session", {"kind": "run_terminal", "termination": "completed"})
    runner = RuntimeRunner(RuntimeOptions(provider=SimpleNamespace(), session_log=log))
    canonical = _LiveCanonicalRuntime()
    calls: list[tuple] = []

    monkeypatch.setattr(runner, "create_canonical_runtime", lambda run_id: canonical)

    async def execute(*args, **kwargs):
        calls.append(args)
        if False:
            yield None

    monkeypatch.setattr(runner, "_execute", execute)
    assert [event async for event in runner.wake("session")] == []
    assert canonical.restored
    assert calls and calls[0][5] is True


@pytest.mark.asyncio
async def test_terminal_projection_without_canonical_journal_fails_closed():
    log = InMemorySessionLog()
    await log.append("session", {
        "kind": "run_started", "run_id": "projection-only", "goal": "goal", "criteria": [],
    })
    await log.append("session", {"kind": "run_terminal", "termination": "completed"})
    runner = RuntimeRunner(RuntimeOptions(provider=SimpleNamespace(), session_log=log))

    with pytest.raises(RuntimeError, match="run_terminal projection has no canonical journal"):
        _ = [event async for event in runner.wake("session")]


@pytest.mark.asyncio
async def test_forged_run_terminal_does_not_suppress_run(monkeypatch):
    log = InMemorySessionLog()
    await log.append("session", {"kind": "run_started", "run_id": "run-1", "goal": "old", "criteria": []})
    await log.append("session", {"kind": "run_terminal", "termination": "completed"})
    runner = RuntimeRunner(RuntimeOptions(provider=SimpleNamespace(), session_log=log))
    canonical = _LiveCanonicalRuntime()
    calls: list[tuple] = []
    monkeypatch.setattr(runner, "create_canonical_runtime", lambda run_id: canonical)

    async def execute(*args, **kwargs):
        calls.append(args)
        if False:
            yield None

    monkeypatch.setattr(runner, "_execute", execute)
    assert [event async for event in runner.run(goal="new", session_id="session")] == []
    assert canonical.restored
    assert calls and calls[0][5] is True


class _FinishAfterToolProvider:
    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        if any(message.role == "tool" for message in context.turns):
            yield TextDelta(delta="resumed-finish")
        else:
            yield TextDelta(delta="fresh-finish")


def _runtime(log: InMemorySessionLog, run_id: str) -> CanonicalRunnerRuntime:
    return CanonicalRunnerRuntime(
        CanonicalKernel(),
        log.kernel_journal,
        f"python-operation-{run_id}",
        max_context_tokens=8_000,
        max_turns=8,
    )


@pytest.mark.asyncio
async def test_restores_pending_agent_tool_effect_from_journal_exactly_once():
    log = InMemorySessionLog()
    run_id = "crash-agent-1"
    session_id = "canonical-agent-wake"
    await log.append(session_id, {
        "kind": "run_started", "run_id": run_id, "goal": "ping then finish", "criteria": [],
    })

    before_crash = _runtime(log, run_id)
    await before_crash.apply_host_event({
        "kind": "set_tools",
        "tools": [{"name": "ping", "description": "ping", "parameters": {"type": "object"}}],
    })
    first = await before_crash.start_agent({"goal": "ping then finish", "criteria": []})
    assert first is not None and first.kind == "call_provider"
    pending = await before_crash.apply_host_event({
        "kind": "provider_result",
        "effect_id": first.effect_id,
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": "call-ping", "name": "ping", "arguments": {}}],
        },
        "stop_reason": "tool_use",
    })
    assert pending is not None and pending.kind == "execute_tool"

    executions = 0

    @tool
    def ping() -> str:
        """Ping."""
        nonlocal executions
        executions += 1
        return "pong"

    runner = RuntimeRunner(RuntimeOptions(
        provider=_FinishAfterToolProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane().register(ping),
        max_tokens=8_000,
        max_turns=8,
    ))

    assert await collect_text(runner.wake(session_id)) == "resumed-finish"
    assert executions == 1


@pytest.mark.asyncio
async def test_restores_pending_workflow_spawn_from_journal_without_session_reconstruction():
    log = InMemorySessionLog()
    run_id = "crash-workflow-1"
    session_id = "canonical-workflow-wake"
    await log.append(session_id, {
        "kind": "run_started", "run_id": run_id, "goal": "workflow:2 nodes", "criteria": [],
    })

    before_crash = _runtime(log, run_id)
    first = await before_crash.start_workflow({
        "nodes": [
            {"task": {"goal": "first"}, "role": "explore", "isolation": "shared", "context_inheritance": "none"},
            {
                "task": {"goal": "second"},
                "role": "plan",
                "isolation": "shared",
                "context_inheritance": "none",
                "depends_on": [0],
            },
        ],
    })
    assert first is not None and first.kind == "spawn_workflow"
    await before_crash.apply_host_event({
        "kind": "workflow_spawn_result",
        "effect_id": first.effect_id,
        "started_agent_ids": ["wf-node0"],
        "failures": [],
    })
    downstream = await before_crash.apply_host_event({
        "kind": "sub_agent_completed",
        "result": {
            "agent_id": "wf-node0",
            "result": {
                "termination": "completed",
                "final_message": {"role": "assistant", "content": "pre-crash dependency output", "tool_calls": []},
                "turns_used": 1,
                "total_tokens_used": 1,
            },
        },
    })
    assert downstream is not None and downstream.kind == "spawn_workflow"

    class _Orchestrator:
        def __init__(self) -> None:
            self.agent_ids: list[str] = []
            self.goals: list[str] = []

        async def run(self, context):
            self.agent_ids.append(context.spec.identity.agent_id)
            self.goals.append(context.spec.goal)
            return SubAgentResult(
                agent_id=context.spec.identity.agent_id,
                result=LoopResult(
                    termination="completed",
                    final_message=Message(role="assistant", content="done"),
                    turns_used=1,
                    total_tokens_used=1,
                ),
            )

    orchestrator = _Orchestrator()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_FinishAfterToolProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        sub_agent_orchestrator=orchestrator,
        max_tokens=8_000,
        max_turns=8,
    ))

    events = [event async for event in runner.wake(session_id)]
    assert orchestrator.agent_ids == ["wf-node1"]
    assert "pre-crash dependency output" in orchestrator.goals[0]
    assert events[-1].type == "done"


def test_production_cutover_sources_do_not_use_legacy_step_entrypoint():
    root = Path(__file__).parents[1] / "deepstrike" / "runtime"
    for name in ("runner.py", "canonical_kernel_step.py"):
        source = (root / name).read_text(encoding="utf-8")
        assert ".step(" not in source
        assert "_kernel_step(" not in source


def test_production_runner_does_not_use_session_log_repair():
    source = (Path(__file__).parents[1] / "deepstrike" / "runtime" / "runner.py").read_text(
        encoding="utf-8"
    )
    assert "repair_events_for_recovery" not in source
