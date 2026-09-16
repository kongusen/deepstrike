from __future__ import annotations

import base64
import json
from pathlib import Path

import pytest

from deepstrike.kernel.canonical import CanonicalKernel
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost, CanonicalRunnerRuntime
from deepstrike.runtime.execution_plane import LocalExecutionPlane
from deepstrike.runtime.kernel_journal import (
    JournalCasConflictError,
    JournalIoError,
    KernelJournal,
)
from deepstrike.runtime.runner import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.runtime.session_log import FileSessionLog
from deepstrike.tools.registry import tool

"""Durable restart recovery — the whole durable path against a **file-backed** journal
(0.2.65 S2), the python mirror of the node suite. Every phase reopens the directory with
a fresh ``FileSessionLog``, the way a restarted process would. Both restore ladders are
exercised — journal-only replay, and checkpoint+tail after the install/ack/reclaim
boundary — plus the two crash windows the host protocol owes byte-identical retries: a
CAS conflict during append (crash-point #8) and a crash between staging the outbound
envelope and its append-ack (drained on wake, adjudication 5e.3).

Equivalence criterion: the recovered run's next step is the step the original input
determined — the committed record embeds exactly the staged envelope content — and the
run reaches the same terminal an uninterrupted twin reaches, with each effect executed
exactly once across the restart.
"""

RUN_ID = "restart-op-1"
OP = f"python-operation-{RUN_ID}"
SESSION = "durable-restart"
FINAL_TEXT = "restart-equivalent-finish"
RUNTIME_OPTIONS = {"max_context_tokens": 8_000, "max_turns": 8}


class _PingThenFinishProvider:
    """Streams the ping tool call until history holds a tool result, then the final text."""

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        if any(message.role == "tool" for message in context.turns):
            yield TextDelta(delta=FINAL_TEXT)
            return
        yield ToolCallEvent(id="call_ping", name="ping", arguments={})


def _new_runtime(journal: KernelJournal) -> CanonicalRunnerRuntime:
    return CanonicalRunnerRuntime(CanonicalKernel(), journal, OP, **RUNTIME_OPTIONS)


def _runner_for(log: FileSessionLog, executions: list[int]) -> RuntimeRunner:
    @tool
    def ping() -> str:
        """Ping."""
        executions[0] += 1
        return "pong"

    return RuntimeRunner(RuntimeOptions(
        provider=_PingThenFinishProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane().register(ping),
        max_tokens=RUNTIME_OPTIONS["max_context_tokens"],
        max_turns=RUNTIME_OPTIONS["max_turns"],
        # The manual drives start with ``exposure_baseline: ["ping"]``; the runner-level
        # run() needs the same surface or the tool call is denied instead of executed.
        baseline_tool_ids=["ping"],
    ))


async def _drive_to_pending_tool_effect(runtime: CanonicalRunnerRuntime) -> None:
    """Drive the operation to a pending ``execute_tool`` effect — the freeze frame of a run
    interrupted between committing the provider turn and executing the requested tool."""
    await runtime.apply_host_event({
        "kind": "set_tools",
        "tools": [{"name": "ping", "description": "Ping", "parameters": {"type": "object"}}],
    })
    first = await runtime.start_agent(
        {"goal": "use ping then finish", "criteria": []},
        {"goal": "use ping then finish", "exposure_baseline": ["ping"]},
    )
    assert first is not None and first.kind == "call_provider"
    pending = await runtime.apply_host_event({
        "kind": "provider_result",
        "effect_id": first.effect_id,
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": "call_ping", "name": "ping", "arguments": {}}],
        },
        "stop_reason": "tool_use",
    })
    assert pending is not None and pending.kind == "execute_tool"


async def _chain(journal: KernelJournal) -> list[tuple[int, str, bytes]]:
    """The journal chain as (step_seq, digest, bytes) triples — bytes are the comparison unit."""
    return [
        (entry.step_seq, entry.record_digest, bytes(entry.record_bytes))
        for entry in await journal.read_from(OP)
    ]


def _envelope_of(record_bytes: bytes) -> dict:
    """A record embeds the envelope that produced it, canonically re-serialized."""
    record = json.loads(record_bytes.decode("utf-8"))
    data = record.get("canonical_input", {}).get("data")
    if not isinstance(data, str):
        raise AssertionError("record embeds no canonical_input envelope")
    return json.loads(base64.b64decode(data).decode("utf-8"))


class _FaultedJournal:
    """A journal decorator that records every staged outbound envelope and fails the Nth
    append. ``compare_and_append`` calls are 1-indexed in order: configure (1), start (2),
    each resolved effect afterwards."""

    def __init__(self, inner: KernelJournal, fault) -> None:
        self._inner = inner
        self._fault = fault
        self._calls = 0
        self.staged: list[str] = []

    async def compare_and_append(self, operation_id, expected_head, record):
        self._calls += 1
        error = self._fault(self._calls)
        if error is not None:
            raise error
        return await self._inner.compare_and_append(operation_id, expected_head, record)

    async def stage_outbound_envelope(self, operation_id: str, envelope_json: str) -> None:
        self.staged.append(envelope_json)
        await self._inner.stage_outbound_envelope(operation_id, envelope_json)

    def __getattr__(self, name: str):
        return getattr(self._inner, name)


@pytest.mark.asyncio
async def test_uninterrupted_run_fixes_the_terminal_every_restart_must_reproduce(tmp_path: Path):
    log = FileSessionLog(tmp_path / "baseline")
    executions = [0]
    text = await collect_text(_runner_for(log, executions).run(
        goal="use ping then finish", session_id=SESSION,
    ))
    assert text == FINAL_TEXT
    assert executions[0] == 1


@pytest.mark.asyncio
async def test_journal_only_ladder_crash_after_commit_resumes_from_journal_bytes_alone(tmp_path: Path):
    crash_dir = tmp_path / "journal-ladder"
    log_before = FileSessionLog(crash_dir)
    await log_before.append(SESSION, {
        "kind": "run_started", "run_id": RUN_ID, "goal": "use ping then finish", "criteria": [],
    })
    await _drive_to_pending_tool_effect(_new_runtime(log_before.kernel_journal))
    frozen = await _chain(log_before.kernel_journal)
    assert len(frozen) >= 3

    # Process restart: a fresh FileSessionLog reopens the same directory. The frozen prefix
    # must come back byte-identical — the bytes are the whole contract.
    log_after = FileSessionLog(crash_dir)
    assert await _chain(log_after.kernel_journal) == frozen

    executions = [0]
    assert await collect_text(_runner_for(log_after, executions).wake(SESSION)) == FINAL_TEXT
    assert executions[0] == 1
    resumed = await _chain(log_after.kernel_journal)
    assert resumed[:len(frozen)] == frozen
    assert len(resumed) > len(frozen)


@pytest.mark.asyncio
async def test_checkpoint_tail_ladder_crash_after_install_ack_reclaim_restores_through_the_checkpoint(tmp_path: Path):
    crash_dir = tmp_path / "checkpoint-ladder"
    log_before = FileSessionLog(crash_dir)
    await log_before.append(SESSION, {
        "kind": "run_started", "run_id": RUN_ID, "goal": "use ping then finish", "criteria": [],
    })
    kernel = CanonicalKernel()
    runtime = CanonicalRunnerRuntime(kernel, log_before.kernel_journal, OP, **RUNTIME_OPTIONS)
    await _drive_to_pending_tool_effect(runtime)
    frozen = await _chain(log_before.kernel_journal)

    # The §12.3 boundary — install, ack, reclaim — runs before the process dies. The pending
    # tool effect lives inside the checkpoint; the covered prefix is reclaimed.
    installed = await CanonicalKernelHost(kernel, log_before.kernel_journal, OP).checkpoint()
    assert installed.acknowledged is True
    retained = await _chain(log_before.kernel_journal)
    assert len(retained) < len(frozen)

    log_after = FileSessionLog(crash_dir)
    reopened = await log_after.kernel_journal.latest_checkpoint(OP)
    assert reopened is not None and reopened.acknowledged is True
    assert await _chain(log_after.kernel_journal) == retained

    # The wake restore takes the checkpoint+tail ladder: latest_checkpoint + records_after(
    # covered_head). The reclaimed prefix never returns; the run continues past it.
    executions = [0]
    assert await collect_text(_runner_for(log_after, executions).wake(SESSION)) == FINAL_TEXT
    assert executions[0] == 1
    resumed = await _chain(log_after.kernel_journal)
    assert resumed[:len(retained)] == retained
    assert all(entry[0] > installed.through_step_seq for entry in resumed)


@pytest.mark.asyncio
async def test_cas_conflict_during_append_rebuilds_and_retries_the_identical_envelope(tmp_path: Path):
    crash_dir = tmp_path / "cas-conflict"
    log = FileSessionLog(crash_dir)
    await log.append(SESSION, {
        "kind": "run_started", "run_id": RUN_ID, "goal": "use ping then finish", "criteria": [],
    })
    faulted = _FaultedJournal(
        log.kernel_journal,
        lambda call: JournalCasConflictError("another writer took this position") if call == 3 else None,
    )
    runtime = CanonicalRunnerRuntime(CanonicalKernel(), faulted, OP, **RUNTIME_OPTIONS)

    # Configure and start land cleanly; the effect resolution hits a stale expected head.
    await runtime.apply_host_event({
        "kind": "set_tools",
        "tools": [{"name": "ping", "description": "Ping", "parameters": {"type": "object"}}],
    })
    first = await runtime.start_agent(
        {"goal": "use ping then finish", "criteria": []},
        {"goal": "use ping then finish", "exposure_baseline": ["ping"]},
    )
    assert first is not None and first.kind == "call_provider"
    before = await _chain(log.kernel_journal)

    # The host must absorb the conflict: abort the prepared step, rebuild from the journal,
    # retry the same bytes. The caller sees an ordinary success.
    pending = await runtime.apply_host_event({
        "kind": "provider_result",
        "effect_id": first.effect_id,
        "message": {
            "role": "assistant",
            "content": "",
            "tool_calls": [{"id": "call_ping", "name": "ping", "arguments": {}}],
        },
        "stop_reason": "tool_use",
    })
    assert pending is not None and pending.kind == "execute_tool"

    # The committed record embeds the envelope staged before the conflict — the retry was
    # the same input, so the record digest is the one the un-conflicted writer determined.
    staged = faulted.staged[-1]
    after = await _chain(log.kernel_journal)
    assert len(after) == len(before) + 1
    assert _envelope_of((await log.kernel_journal.read_from(OP))[-1].record_bytes) == json.loads(staged)


@pytest.mark.asyncio
async def test_crash_between_staging_the_envelope_and_its_append_drains_identically_on_wake(tmp_path: Path):
    crash_dir = tmp_path / "crash-window"
    log = FileSessionLog(crash_dir)
    await log.append(SESSION, {
        "kind": "run_started", "run_id": RUN_ID, "goal": "use ping then finish", "criteria": [],
    })
    faulted = _FaultedJournal(
        log.kernel_journal,
        lambda call: JournalIoError("simulated crash after stage") if call == 3 else None,
    )
    runtime = CanonicalRunnerRuntime(CanonicalKernel(), faulted, OP, **RUNTIME_OPTIONS)
    await runtime.apply_host_event({
        "kind": "set_tools",
        "tools": [{"name": "ping", "description": "Ping", "parameters": {"type": "object"}}],
    })
    first = await runtime.start_agent(
        {"goal": "use ping then finish", "criteria": []},
        {"goal": "use ping then finish", "exposure_baseline": ["ping"]},
    )
    assert first is not None and first.kind == "call_provider"

    # The storage layer dies after staging the resolve envelope but before its append-ack:
    # the record never lands, the effect stays pending in the durable state, and the staged
    # bytes survive (append-before failures must not clear them).
    with pytest.raises(JournalIoError, match="simulated crash after stage"):
        await runtime.apply_host_event({
            "kind": "provider_result",
            "effect_id": first.effect_id,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"id": "call_ping", "name": "ping", "arguments": {}}],
            },
            "stop_reason": "tool_use",
        })
    staged = faulted.staged[-1]
    assert len(await _chain(log.kernel_journal)) == 2
    assert await log.kernel_journal.read_outbound_envelope(OP) == staged

    # Restart: wake rebuilds from the journal, drains the staged envelope identically (the
    # resolved record embeds exactly that input), and the run continues to the same terminal
    # with the effect executed exactly once.
    log_after = FileSessionLog(crash_dir)
    assert await log_after.kernel_journal.read_outbound_envelope(OP) == staged
    executions = [0]
    assert await collect_text(_runner_for(log_after, executions).wake(SESSION)) == FINAL_TEXT
    assert executions[0] == 1
    after = await _chain(log_after.kernel_journal)
    assert len(after) >= 3
    assert after[2][0] == 2
    assert _envelope_of((await log_after.kernel_journal.read_from(OP))[2].record_bytes) == json.loads(staged)
