import json
from pathlib import Path

import pytest

from deepstrike.memory import (
    FileMemoryLifecycleStore, MemoryLease, MemoryScope, MemoryWriteBudget, consume_memory_lease_write,
    memory_record_expired, revoke_memory_lease, verify_memory_lease,
)


def test_memory_lease_enforces_scope_expiry_revocation_and_budget():
    lease = MemoryLease("l", "a", MemoryScope("t", "n"), 0, 100, 1)
    assert verify_memory_lease(lease, "a", lease.scope, 50) == (True, None)
    assert verify_memory_lease(lease, "b", lease.scope, 50)[1] == "lease_scope_mismatch"
    assert verify_memory_lease(lease, "a", lease.scope, 100)[1] == "lease_expired"
    assert verify_memory_lease(revoke_memory_lease(lease, 40), "a", lease.scope, 50)[1] == "lease_revoked"
    assert verify_memory_lease(consume_memory_lease_write(lease), "a", lease.scope, 50)[1] == "memory_write_budget_exhausted"


def test_memory_ttl_uses_updated_at_and_day_precision():
    record = type("Record", (), {"updated_at": 1_000, "ttl_days": 1})()
    assert not memory_record_expired(record, 1_000 + 86_400_000)
    assert memory_record_expired(record, 1_000 + 86_400_001)


@pytest.mark.asyncio
async def test_file_lifecycle_store_is_idempotent_and_survives_restart(tmp_path: Path):
    path = tmp_path / "lifecycle.json"
    lease = MemoryLease("lease-1", "agent-1", MemoryScope("tenant-1", "ns-1"), 10, 1_000, 1)
    store = FileMemoryLifecycleStore(path, now=lambda: 100)
    await store.issue_lease(lease)
    first = await store.consume_write("agent-1", lease.scope, "op-1", 100)
    assert first["valid"] is True
    assert await store.consume_write("agent-1", lease.scope, "op-1", 100) == first

    restarted = FileMemoryLifecycleStore(path, now=lambda: 100)
    assert (await restarted.get_lease("agent-1", lease.scope)).writes_used == 1
    assert await restarted.consume_write("agent-1", lease.scope, "op-2", 100) == {
        "valid": False, "reason_code": "memory_write_budget_exhausted",
    }
    await restarted.renew_lease("agent-1", "lease-1", 2_000, 200)
    await restarted.revoke_lease("agent-1", "lease-1", 300)
    assert [event["type"] for event in await restarted.events()] == [
        "lease_issued", "lease_write_consumed", "lease_renewed", "lease_revoked",
    ]


@pytest.mark.asyncio
async def test_file_lifecycle_deletion_is_request_then_apply_and_retries(tmp_path: Path):
    attempts = 0

    async def delete_physical(agent_id, scope, record_id):
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise RuntimeError("simulated crash")
        return True

    scope = MemoryScope("tenant-1", "ns-1")
    store = FileMemoryLifecycleStore(tmp_path / "lifecycle.json", now=lambda: 100, delete_physical=delete_physical)
    with pytest.raises(RuntimeError, match="simulated crash"):
        await store.delete_record("agent-1", scope, "record-1", "delete-1")
    assert [event["type"] for event in await store.events()] == ["deletion_requested"]
    receipt = await store.delete_record("agent-1", scope, "record-1", "delete-1")
    assert receipt["status"] == "applied" and receipt["event_sequence"] == 1
    assert [event["type"] for event in await store.events()] == ["deletion_requested", "deletion_applied"]
    assert await store.delete_record("agent-1", scope, "record-1", "delete-1") == receipt
    assert await store.deleted("agent-1") == [receipt]


@pytest.mark.asyncio
async def test_file_lifecycle_store_rejects_corrupt_snapshot(tmp_path: Path):
    path = tmp_path / "lifecycle.json"
    path.write_text(json.dumps({}))
    store = FileMemoryLifecycleStore(path)
    with pytest.raises(ValueError, match="invalid memory lifecycle snapshot"):
        await store.events()
    assert path.read_text() == "{}"
