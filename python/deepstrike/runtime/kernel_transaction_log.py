from __future__ import annotations

import hashlib
import json
import math
import struct
from typing import Any, TypedDict


KERNEL_LOG_RECORD_VERSION = 1


class KernelOperationGenesisBody(TypedDict):
    record_version: int
    abi_version: int
    operation_id: str
    initial_scheduler_policy: dict[str, Any]
    resolved_runtime_defaults: dict[str, Any]
    default_policy_version: int


class KernelOperationGenesis(KernelOperationGenesisBody):
    genesis_digest: str


class KernelTransactionBody(TypedDict):
    record_version: int
    operation_id: str
    step_seq: int
    base_generation: int
    input: dict[str, Any]
    input_digest: str
    previous_transaction_digest: str
    step_digest: str


class KernelTransaction(KernelTransactionBody):
    transaction_digest: str


class KernelGenesisReceipt(TypedDict):
    log_seq: int
    genesis_digest: str


class DurableAppendReceipt(TypedDict):
    log_seq: int
    transaction_digest: str


class KernelTransactionEntry(TypedDict):
    log_seq: int
    transaction: KernelTransaction


class KernelOperationCursor(TypedDict):
    operation_id: str
    next_event_sequence: int
    next_step_seq: int
    transaction_head_digest: str


class KernelLogConflictError(RuntimeError):
    pass


class KernelLogIntegrityError(ValueError):
    pass


def canonical_kernel_json(value: Any) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, int):
        if abs(value) > 9_007_199_254_740_991:
            raise KernelLogIntegrityError("canonical record integer exceeds the cross-SDK safe range")
        return str(value)
    if isinstance(value, float):
        if not math.isfinite(value):
            raise KernelLogIntegrityError("canonical records require finite numbers")
        if value.is_integer():
            integer = int(value)
            if abs(integer) > 9_007_199_254_740_991:
                raise KernelLogIntegrityError("canonical record integer exceeds the cross-SDK safe range")
            return str(integer)
        return "f64:" + struct.pack(">d", value).hex()
    if isinstance(value, list):
        return "[" + ",".join(canonical_kernel_json(item) for item in value) + "]"
    if isinstance(value, dict):
        if not all(isinstance(key, str) for key in value):
            raise KernelLogIntegrityError("canonical record object keys must be strings")
        return "{" + ",".join(
            f"{json.dumps(key, ensure_ascii=False, separators=(',', ':'))}:{canonical_kernel_json(value[key])}"
            for key in sorted(value)
        ) + "}"
    raise KernelLogIntegrityError(f"unsupported canonical record value: {type(value).__name__}")


def kernel_record_digest(value: Any) -> str:
    return hashlib.sha256(canonical_kernel_json(value).encode("utf-8")).hexdigest()


def create_kernel_operation_genesis(
    *,
    abi_version: int,
    operation_id: str,
    initial_scheduler_policy: dict[str, Any],
    resolved_runtime_defaults: dict[str, Any],
    default_policy_version: int,
) -> KernelOperationGenesis:
    body: KernelOperationGenesisBody = {
        "record_version": KERNEL_LOG_RECORD_VERSION,
        "abi_version": abi_version,
        "operation_id": operation_id,
        "initial_scheduler_policy": initial_scheduler_policy,
        "resolved_runtime_defaults": resolved_runtime_defaults,
        "default_policy_version": default_policy_version,
    }
    _validate_genesis_body(body)
    return KernelOperationGenesis(**body, genesis_digest=kernel_record_digest(body))


def create_kernel_transaction(
    *,
    operation_id: str,
    step_seq: int,
    base_generation: int,
    input: dict[str, Any],
    step: dict[str, Any],
    previous_transaction_digest: str,
) -> KernelTransaction:
    body: KernelTransactionBody = {
        "record_version": KERNEL_LOG_RECORD_VERSION,
        "operation_id": operation_id,
        "step_seq": step_seq,
        "base_generation": base_generation,
        "input": input,
        "input_digest": kernel_record_digest(input),
        "previous_transaction_digest": previous_transaction_digest,
        "step_digest": kernel_record_digest(step),
    }
    _validate_transaction_body(body)
    return KernelTransaction(**body, transaction_digest=kernel_record_digest(body))


def verify_kernel_operation_genesis(genesis: KernelOperationGenesis) -> None:
    body = {key: value for key, value in genesis.items() if key != "genesis_digest"}
    _validate_genesis_body(body)  # type: ignore[arg-type]
    if kernel_record_digest(body) != genesis.get("genesis_digest"):
        raise KernelLogIntegrityError("kernel genesis digest does not match its canonical body")


def verify_kernel_transaction(transaction: KernelTransaction) -> None:
    body = {key: value for key, value in transaction.items() if key != "transaction_digest"}
    _validate_transaction_body(body)  # type: ignore[arg-type]
    if kernel_record_digest(body["input"]) != body["input_digest"]:
        raise KernelLogIntegrityError("kernel transaction input digest does not match its input")
    if kernel_record_digest(body) != transaction.get("transaction_digest"):
        raise KernelLogIntegrityError("kernel transaction digest does not match its canonical body")


def verify_kernel_transaction_successor(
    previous: KernelTransaction | None,
    transaction: KernelTransaction,
) -> None:
    expected_step_seq = previous["step_seq"] + 1 if previous else 1
    expected_generation = previous["base_generation"] + 1 if previous else 0
    if transaction["step_seq"] != expected_step_seq:
        raise KernelLogIntegrityError(
            f"kernel transaction step_seq {transaction['step_seq']} does not follow {expected_step_seq - 1}"
        )
    if transaction["base_generation"] != expected_generation:
        raise KernelLogIntegrityError(
            "kernel transaction base_generation "
            f"{transaction['base_generation']} does not match {expected_generation}"
        )
    if transaction["input"].get("operation_id") != transaction["operation_id"]:
        raise KernelLogIntegrityError(
            "kernel transaction input operation_id does not match its envelope"
        )


def verify_kernel_transaction_stream(
    genesis: KernelOperationGenesis,
    transactions: list[KernelTransaction],
) -> KernelOperationCursor:
    """Validate a complete authoritative stream and derive its next wire cursor."""
    verify_kernel_operation_genesis(genesis)
    previous: KernelTransaction | None = None
    head = genesis["genesis_digest"]

    for transaction in transactions:
        verify_kernel_transaction(transaction)
        if transaction["operation_id"] != genesis["operation_id"]:
            raise KernelLogIntegrityError(
                "kernel transaction operation_id does not match genesis"
            )
        if transaction["previous_transaction_digest"] != head:
            raise KernelLogIntegrityError(
                "kernel transaction digest chain is not continuous"
            )
        verify_kernel_transaction_successor(previous, transaction)
        previous = transaction
        head = transaction["transaction_digest"]

    next_sequence = (previous["step_seq"] if previous else 0) + 1
    return {
        "operation_id": genesis["operation_id"],
        "next_event_sequence": next_sequence,
        "next_step_seq": next_sequence,
        "transaction_head_digest": head,
    }


def _validate_genesis_body(genesis: KernelOperationGenesisBody) -> None:
    if genesis.get("record_version") != KERNEL_LOG_RECORD_VERSION:
        raise KernelLogIntegrityError("unsupported kernel genesis record version")
    if not _positive_safe_integer(genesis.get("abi_version")):
        raise KernelLogIntegrityError("kernel genesis abi_version must be a positive safe integer")
    if not genesis.get("operation_id"):
        raise KernelLogIntegrityError("kernel genesis operation_id is required")
    if not _positive_safe_integer(genesis.get("default_policy_version")):
        raise KernelLogIntegrityError("kernel genesis default_policy_version must be a positive safe integer")


def _validate_transaction_body(transaction: KernelTransactionBody) -> None:
    if transaction.get("record_version") != KERNEL_LOG_RECORD_VERSION:
        raise KernelLogIntegrityError("unsupported kernel transaction record version")
    if not transaction.get("operation_id"):
        raise KernelLogIntegrityError("kernel transaction operation_id is required")
    if not _positive_safe_integer(transaction.get("step_seq")):
        raise KernelLogIntegrityError("kernel transaction step_seq must be a positive safe integer")
    generation = transaction.get("base_generation")
    if (
        not isinstance(generation, int)
        or isinstance(generation, bool)
        or generation < 0
        or generation > 9_007_199_254_740_991
    ):
        raise KernelLogIntegrityError("kernel transaction base_generation must be a non-negative safe integer")
    if not transaction.get("previous_transaction_digest"):
        raise KernelLogIntegrityError("kernel transaction previous_transaction_digest is required")


def _positive_safe_integer(value: Any) -> bool:
    return (
        isinstance(value, int)
        and not isinstance(value, bool)
        and 0 < value <= 9_007_199_254_740_991
    )
