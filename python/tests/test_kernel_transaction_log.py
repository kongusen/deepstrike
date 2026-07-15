from __future__ import annotations

import pytest

from deepstrike.runtime.kernel_transaction_log import (
    KernelLogConflictError,
    KernelLogIntegrityError,
    canonical_kernel_json,
    create_kernel_operation_genesis,
    create_kernel_transaction,
    kernel_record_digest,
)
from deepstrike.runtime.session_log import FileSessionLog, InMemorySessionLog


def genesis(operation_id: str = "op-python"):
    return create_kernel_operation_genesis(
        abi_version=2,
        operation_id=operation_id,
        initial_scheduler_policy={"max_tokens": 8_000},
        resolved_runtime_defaults={"max_input_bytes": 16_777_216},
        default_policy_version=1,
    )


def transaction(previous_digest: str, step_seq: int = 1):
    return create_kernel_transaction(
        operation_id="op-python",
        step_seq=step_seq,
        base_generation=step_seq - 1,
        input={"version": 2, "operation_id": "op-python", "event_id": f"event-{step_seq}"},
        step={"version": 2, "operation_id": "op-python", "step_seq": step_seq, "actions": []},
        previous_transaction_digest=previous_digest,
    )


def test_canonical_kernel_json_sorts_keys_and_rejects_binary_floats():
    assert canonical_kernel_json({"z": 1, "a": [True, "雪"]}) == '{"a":[true,"雪"],"z":1}'
    assert kernel_record_digest({"z": 1, "a": [True, "雪"]}) == (
        "74ffaa09c9570f87244813a5b15514369f7b1a8996e3e80017585b4df246c1f7"
    )
    with pytest.raises(KernelLogIntegrityError):
        canonical_kernel_json({"ratio": 0.5})


@pytest.mark.asyncio
async def test_in_memory_kernel_transactions_are_cas_fenced_and_tamper_evident():
    log = InMemorySessionLog()
    operation_genesis = genesis()
    await log.append_kernel_genesis("session", operation_genesis)
    await log.append("session", {"kind": "run_started", "run_id": "run", "goal": "a", "criteria": []})
    first = transaction(operation_genesis["genesis_digest"])
    receipt = await log.compare_and_append_kernel_transaction(
        "session", operation_genesis["genesis_digest"], first
    )

    stale = transaction(operation_genesis["genesis_digest"], 2)
    with pytest.raises(KernelLogConflictError):
        await log.compare_and_append_kernel_transaction(
            "session", operation_genesis["genesis_digest"], stale
        )
    assert await log.kernel_transaction_head("session") == first["transaction_digest"]
    assert await log.read_kernel_transactions("session") == [
        {"log_seq": receipt["log_seq"], "transaction": first}
    ]

    tampered = {**first, "input": {**first["input"], "event_id": "tampered"}}
    with pytest.raises(KernelLogIntegrityError):
        await log.compare_and_append_kernel_transaction(
            "session", first["transaction_digest"], tampered
        )


@pytest.mark.asyncio
async def test_file_kernel_transaction_stream_survives_reopen(tmp_path):
    log = FileSessionLog(tmp_path)
    operation_genesis = genesis()
    genesis_receipt = await log.append_kernel_genesis("session", operation_genesis)
    await log.append("session", {"kind": "run_started", "run_id": "run", "goal": "a", "criteria": []})
    first = transaction(operation_genesis["genesis_digest"])
    receipt = await log.compare_and_append_kernel_transaction(
        "session", operation_genesis["genesis_digest"], first
    )

    reopened = FileSessionLog(tmp_path)
    assert await reopened.read_kernel_genesis("session") == operation_genesis
    assert await reopened.read_kernel_transactions("session") == [
        {"log_seq": receipt["log_seq"], "transaction": first}
    ]
    assert await reopened.kernel_transaction_head("session") == first["transaction_digest"]
    assert genesis_receipt["log_seq"] == 0
    assert receipt["log_seq"] == 2
