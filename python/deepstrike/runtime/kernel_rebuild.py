from __future__ import annotations

import json
from typing import Any, TypedDict

from deepstrike.runtime.kernel_transaction_log import (
    KernelLogIntegrityError,
    KernelOperationCursor,
    KernelOperationGenesis,
    KernelTransaction,
    kernel_record_digest,
    verify_kernel_transaction_stream,
)


class KernelRebuildResult(TypedDict):
    runtime: Any
    cursor: KernelOperationCursor


def rebuild_kernel_runtime(
    runtime: Any,
    genesis: KernelOperationGenesis,
    transactions: list[KernelTransaction],
) -> KernelRebuildResult:
    """Deterministically fold an authoritative stream into a fresh Python runtime."""
    cursor = verify_kernel_transaction_stream(genesis, transactions)
    _assert_fresh_runtime_matches_genesis(runtime, genesis)

    for transaction in transactions:
        prepared = json.loads(runtime.prepare_step(json.dumps(transaction["input"])))
        token = prepared.get("prepare_token")
        try:
            if prepared.get("status") != "prepared" or not token:
                raise KernelLogIntegrityError(
                    f"kernel rebuild step {transaction['step_seq']} was not accepted as a new transition"
                )
            if prepared.get("base_generation") != transaction["base_generation"]:
                raise KernelLogIntegrityError(
                    f"kernel rebuild generation diverged at step {transaction['step_seq']}"
                )
            if prepared.get("step", {}).get("step_seq") != transaction["step_seq"]:
                raise KernelLogIntegrityError(
                    f"kernel rebuild step_seq diverged at step {transaction['step_seq']}"
                )
            if kernel_record_digest(prepared["input"]) != transaction["input_digest"]:
                raise KernelLogIntegrityError(
                    f"kernel rebuild normalized input diverged at step {transaction['step_seq']}"
                )
            if kernel_record_digest(prepared["step"]) != transaction["step_digest"]:
                raise KernelLogIntegrityError(
                    f"kernel rebuild planned step diverged at step {transaction['step_seq']}"
                )

            committed = json.loads(runtime.commit_prepared(token))
            if kernel_record_digest(committed) != transaction["step_digest"]:
                raise KernelLogIntegrityError(
                    f"kernel rebuild committed step diverged at step {transaction['step_seq']}"
                )
        except Exception:
            if token and prepared.get("status") == "prepared":
                try:
                    runtime.abort_prepared(token)
                except Exception:
                    pass
            raise

    return {"runtime": runtime, "cursor": cursor}


def _assert_fresh_runtime_matches_genesis(
    runtime: Any,
    genesis: KernelOperationGenesis,
) -> None:
    snapshot = json.loads(runtime.snapshot())
    if snapshot.get("abi_version") != genesis["abi_version"]:
        raise KernelLogIntegrityError(
            "kernel runtime ABI version does not match operation genesis"
        )
    if genesis["default_policy_version"] != 1:
        raise KernelLogIntegrityError(
            "kernel operation uses an unsupported default policy version"
        )
    if (
        snapshot.get("operation_id")
        or snapshot.get("next_step_seq") != 1
        or snapshot.get("accepted_inputs") != []
    ):
        raise KernelLogIntegrityError("kernel rebuild requires a fresh runtime")
    if kernel_record_digest(snapshot["initial_policy"]) != kernel_record_digest(
        genesis["initial_scheduler_policy"]
    ):
        raise KernelLogIntegrityError(
            "kernel runtime initial policy does not match operation genesis"
        )
    defaults = {
        "snapshot_version": snapshot["snapshot_version"],
        "snapshot_input_limit": snapshot["snapshot_input_limit"],
        "max_input_bytes": snapshot["max_input_bytes"],
        "snapshot_journal_bytes_limit": snapshot["snapshot_journal_bytes_limit"],
    }
    if kernel_record_digest(defaults) != kernel_record_digest(
        genesis["resolved_runtime_defaults"]
    ):
        raise KernelLogIntegrityError(
            "kernel runtime defaults do not match operation genesis"
        )
