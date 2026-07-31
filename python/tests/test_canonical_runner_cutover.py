from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest

from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner
from deepstrike.runtime.session_log import InMemorySessionLog


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


def test_production_cutover_sources_do_not_use_legacy_step_entrypoint():
    root = Path(__file__).parents[1] / "deepstrike" / "runtime"
    for name in ("runner.py", "canonical_kernel_step.py"):
        source = (root / name).read_text(encoding="utf-8")
        assert ".step(" not in source
        assert "_kernel_step(" not in source
