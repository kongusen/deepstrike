from __future__ import annotations

import pytest

from deepstrike.runtime.kernel_transaction_log import (
    KernelLogConflictError,
    KernelLogIntegrityError,
    canonical_kernel_json,
    create_kernel_operation_genesis,
    create_kernel_transaction,
    kernel_record_digest,
    verify_kernel_transaction_stream,
)
from deepstrike.runtime.session_log import FileSessionLog, InMemorySessionLog
from deepstrike.runtime.kernel_rebuild import rebuild_kernel_runtime


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


def test_validates_digest_chain_and_derives_regex_free_operation_cursor():
    operation_genesis = genesis()
    first = transaction(operation_genesis["genesis_digest"])
    second = transaction(first["transaction_digest"], 2)

    assert verify_kernel_transaction_stream(operation_genesis, [first, second]) == {
        "operation_id": "op-python",
        "next_event_sequence": 3,
        "next_step_seq": 3,
        "transaction_head_digest": second["transaction_digest"],
    }
    with pytest.raises(KernelLogIntegrityError):
        verify_kernel_transaction_stream(operation_genesis, [second, first])


def test_deterministically_rebuilds_fresh_runtime_from_committed_transactions():
    phases: list[str] = []
    operation_genesis = create_kernel_operation_genesis(
        abi_version=2,
        operation_id="op-python",
        initial_scheduler_policy={"max_tokens": 8_000},
        resolved_runtime_defaults={
            "snapshot_version": 2,
            "snapshot_input_limit": 10_000,
            "max_input_bytes": 16_777_216,
            "snapshot_journal_bytes_limit": 67_108_864,
        },
        default_policy_version=1,
    )
    input_value = {"version": 2, "operation_id": "op-python", "event_id": "opaque"}
    step = {
        "version": 2,
        "operation_id": "op-python",
        "input_event_id": "opaque",
        "step_seq": 1,
        "actions": [],
        "observations": [],
        "faults": [],
    }
    committed = create_kernel_transaction(
        operation_id="op-python",
        step_seq=1,
        base_generation=0,
        input=input_value,
        step=step,
        previous_transaction_digest=operation_genesis["genesis_digest"],
    )

    class Runtime:
        def snapshot(self):
            import json
            return json.dumps({
                "snapshot_version": 2,
                "abi_version": 2,
                "initial_policy": {"max_tokens": 8_000},
                "lifecycle": "created",
                "next_step_seq": 1,
                "snapshot_input_limit": 10_000,
                "max_input_bytes": 16_777_216,
                "snapshot_journal_bytes_limit": 67_108_864,
                "accepted_input_bytes": 0,
                "accepted_inputs": [],
            })

        def prepare_step(self, input_json):
            import json
            phases.append("prepare")
            return json.dumps({
                "status": "prepared",
                "base_generation": 0,
                "prepare_token": "token",
                "input": json.loads(input_json),
                "step": step,
            })

        def commit_prepared(self, token):
            import json
            assert token == "token"
            phases.append("commit")
            return json.dumps(step)

        def abort_prepared(self, _token):
            phases.append("abort")

    rebuilt = rebuild_kernel_runtime(Runtime(), operation_genesis, [committed])
    assert phases == ["prepare", "commit"]
    assert rebuilt["cursor"]["next_event_sequence"] == 2
    assert rebuilt["cursor"]["transaction_head_digest"] == committed["transaction_digest"]


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
    assert await log.kernel_transaction_head("session", "op-python") == first["transaction_digest"]
    assert await log.read_kernel_transactions("session", "op-python") == [
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
    assert await reopened.read_kernel_genesis("session", "op-python") == operation_genesis
    assert await reopened.read_kernel_transactions("session", "op-python") == [
        {"log_seq": receipt["log_seq"], "transaction": first}
    ]
    assert await reopened.kernel_transaction_head("session", "op-python") == first["transaction_digest"]
    assert genesis_receipt["log_seq"] == 0
    assert receipt["log_seq"] == 2
