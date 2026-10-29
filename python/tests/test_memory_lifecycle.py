from deepstrike.memory import (
    MemoryLease, MemoryScope, MemoryWriteBudget, consume_memory_lease_write,
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
