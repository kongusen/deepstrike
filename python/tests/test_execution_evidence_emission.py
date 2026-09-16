"""P4-S3a (0.2.64 S1): python runner execution-evidence emission parity with node.

One real run must land the full chain in SessionLog:
run_started.route → prompt_measured.effect_id → provider_attempt (one per terminal exit:
success / transport_exhausted / aborted / rejected) → llm_completed(effect_id, invocation_id,
wire_evidence). Everything asserted here is L2 host evidence (B7) — never kernel input.
"""

import json
from types import SimpleNamespace

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.stream import TextDelta, UsageEvent
from deepstrike.runtime.execution_evidence import (
    FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY,
    provider_attempt_to_record,
    route_to_record,
    try_normalize_provider_usage,
    usage_to_record,
)
from deepstrike.providers.usage import ProviderUsage
from deepstrike.runtime.session_log import FileSessionLog


class _OneTurnProvider:
    """Answers turn 1 with a usage frame + text, then ends the run."""

    def __init__(self) -> None:
        self.calls = 0

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        self.calls += 1
        yield UsageEvent(total_tokens=30, input_tokens=20, output_tokens=10)
        yield TextDelta(delta="done")


class _FailingProvider:
    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="partial")
        raise RuntimeError("boom")


class _LongStreamProvider:
    def __init__(self) -> None:
        self.runner = None

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="first")
        for _ in range(1000):
            yield TextDelta(delta="later")


class _OversizedPromptProvider:
    """A native-exact measurement far over budget → rejected before any transport."""

    async def complete(self, context, tools, extensions=None):
        raise NotImplementedError

    async def count_tokens(self, context, tools, extensions=None):
        return SimpleNamespace(input_tokens=10**9, source={"kind": "native"}, confidence="exact")

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="never reached")


def _events(log, session_id):
    return log.read(session_id)


def _kinds(entries, kind):
    return [entry.event for entry in entries if entry.event["kind"] == kind]


@pytest.mark.asyncio
async def test_a_successful_turn_lands_the_full_evidence_chain():
    log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_OneTurnProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=2,
    ))

    async for _ in runner.run(goal="say done", session_id="evidence-success"):
        pass

    entries = await _events(log, "evidence-success")

    started = _kinds(entries, "run_started")
    assert len(started) == 1
    route = started[0].get("route")
    assert isinstance(route, dict) and route["route_id"].startswith("sha256:")
    assert route["endpoint"]["id"]

    measured = _kinds(entries, "prompt_measured")
    assert measured, "a prompt measurement must be recorded"
    assert all(m.get("effect_id") for m in measured)
    fingerprint = measured[0]["measurement"]["request_fingerprint"]

    attempts = _kinds(entries, "provider_attempt")
    assert len(attempts) == 1
    attempt = attempts[0]
    assert attempt["status"] == "success"
    assert attempt["effect_id"] == measured[0]["effect_id"]
    assert attempt["request_fingerprint"] == fingerprint
    assert attempt["route"]["route_id"] == route["route_id"]
    assert attempt["attempt_seq"] == 1
    assert attempt["transport_rungs"] == 1
    # The policy id pins only because a measurement exists (P4 §2.1).
    assert attempt["accounting_policy_id"] == FULL_FOOTPRINT_USAGE_ACCOUNTING_POLICY.policy_id
    assert attempt["usage"]["input_tokens"] == 20
    assert attempt["usage"]["output_tokens"] == 10
    assert attempt["wire_evidence"]["request_fingerprint"] == fingerprint
    assert attempt["started_at_ms"] <= attempt["finished_at_ms"]

    completed = _kinds(entries, "llm_completed")
    assert len(completed) == 1
    assert completed[0]["effect_id"] == attempt["effect_id"]
    # First-try success: the invocation id IS the chain's only effect id (P4 §1.1).
    assert completed[0]["invocation_id"] == attempt["effect_id"]
    assert completed[0]["wire_evidence"]["request_fingerprint"] == fingerprint


@pytest.mark.asyncio
async def test_a_transport_exhaustion_lands_a_failed_attempt_with_error_class():
    log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_FailingProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=1,
    ))

    async for _ in runner.run(goal="explode", session_id="evidence-transport"):
        pass

    entries = await _events(log, "evidence-transport")
    attempts = _kinds(entries, "provider_attempt")
    assert len(attempts) == 1
    assert attempts[0]["status"] == "transport_exhausted"
    assert attempts[0]["transport_rungs"] == 1
    # The error CLASS only — never the raw vendor text (B1).
    assert attempts[0]["last_error_class"]
    assert "boom" not in json.dumps(attempts[0])
    assert _kinds(entries, "llm_completed") == []


@pytest.mark.asyncio
async def test_a_budget_rejected_request_lands_a_zero_rung_attempt():
    log = InMemorySessionLog()
    runner = RuntimeRunner(RuntimeOptions(
        provider=_OversizedPromptProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=1024,
        max_turns=1,
    ))

    async for _ in runner.run(goal="too big", session_id="evidence-rejected"):
        pass

    entries = await _events(log, "evidence-rejected")
    attempts = _kinds(entries, "provider_attempt")
    assert len(attempts) >= 1
    rejected = attempts[0]
    assert rejected["status"] == "rejected"
    assert rejected["transport_rungs"] == 0
    assert rejected["last_error_class"] == "context_overflow"
    # G2: the fingerprint still binds the would-be request to its prompt_measured record.
    measured = _kinds(entries, "prompt_measured")
    assert measured and rejected["request_fingerprint"] == measured[0]["measurement"]["request_fingerprint"]


@pytest.mark.asyncio
async def test_a_host_interruption_lands_an_aborted_attempt():
    log = InMemorySessionLog()
    provider = _LongStreamProvider()
    runner = RuntimeRunner(RuntimeOptions(
        provider=provider,
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=3,
    ))

    async for event in runner.run(goal="cancel me", session_id="evidence-aborted"):
        if isinstance(event, TextDelta):
            runner.interrupt("user")

    entries = await _events(log, "evidence-aborted")
    attempts = _kinds(entries, "provider_attempt")
    assert len(attempts) == 1
    assert attempts[0]["status"] == "aborted"
    assert attempts[0]["effect_id"]
    assert attempts[0]["request_fingerprint"]


@pytest.mark.asyncio
async def test_llm_completed_evidence_fields_survive_the_file_log_roundtrip(tmp_path):
    log = FileSessionLog(tmp_path)
    runner = RuntimeRunner(RuntimeOptions(
        provider=_OneTurnProvider(),
        session_log=log,
        execution_plane=LocalExecutionPlane(),
        max_tokens=2048,
        max_turns=2,
    ))

    async for _ in runner.run(goal="say done", session_id="evidence-roundtrip"):
        pass

    # Re-read through a fresh instance so nothing comes from memory.
    entries = await FileSessionLog(tmp_path).read("evidence-roundtrip")
    completed = _kinds(entries, "llm_completed")
    assert len(completed) == 1
    event = completed[0]
    assert event["effect_id"] and event["invocation_id"] == event["effect_id"]
    assert event["wire_evidence"]["request_fingerprint"]
    attempts = _kinds(entries, "provider_attempt")
    assert len(attempts) == 1 and attempts[0]["status"] == "success"
    started = _kinds(entries, "run_started")
    assert started[0]["route"]["route_id"] == attempts[0]["route"]["route_id"]


@pytest.mark.asyncio
async def test_a_provider_attempt_without_fingerprint_is_rejected_on_read(tmp_path):
    log = FileSessionLog(tmp_path)
    # Forge a 0.2.63-shaped-but-broken record straight onto disk (G2 teeth).
    path = tmp_path / "forged.jsonl"
    path.write_text(json.dumps({
        "seq": 0,
        "event": {
            "kind": "provider_attempt",
            "effect_id": "op:step:1:effect:0",
            "attempt_seq": 1,
            "route": {"route_id": "sha256:x"},
            "status": "success",
            "transport_rungs": 1,
        },
    }) + "\n")
    with pytest.raises(ValueError, match="request_fingerprint"):
        await log.read("forged")


def test_attempt_record_helpers_pin_policy_and_usage():
    route = {
        "route_id": "sha256:r", "provider": "p", "protocol": "anthropic-messages", "model": "m",
        "endpoint": {"id": "p.anthropic-messages", "protocol": "anthropic-messages", "base_url": ""},
        "adapter_version": "0.2.64", "capabilities_ref": "anthropic-messages",
    }
    usage = try_normalize_provider_usage(ProviderUsage(input_tokens=100, output_tokens=40))
    assert usage is not None
    record = provider_attempt_to_record({
        "effect_id": "op:step:1:effect:0",
        "attempt_seq": 1,
        "route": route,
        "request_fingerprint": "fp",
        "status": "success",
        "transport_rungs": 1,
        "started_at_ms": 1,
        "finished_at_ms": 2,
        "usage": usage_to_record(usage),
    }, "deepstrike.full-footprint@2026-09-15")
    assert record["accounting_policy_id"] == "deepstrike.full-footprint@2026-09-15"
    assert record["usage"]["uncached_input_tokens"] == 100
    # An invalid frame (cache subset > input) degrades to no measurement, never a run failure.
    assert try_normalize_provider_usage(
        ProviderUsage(input_tokens=10, output_tokens=5, cache_read_input_tokens=11)
    ) is None
