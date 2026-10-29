"""Shared memory lease, write budget, TTL, and deletion scope rules."""
from __future__ import annotations

from dataclasses import dataclass, replace
from typing import Literal

from .protocols import MemoryRecord, MemoryScope


@dataclass(frozen=True)
class MemoryLease:
    lease_id: str
    agent_id: str
    scope: MemoryScope
    issued_at: int
    expires_at: int
    max_writes: int
    writes_used: int = 0
    revoked_at: int | None = None


MemoryLeaseRejection = Literal[
    "lease_missing", "lease_scope_mismatch", "lease_expired", "lease_revoked",
    "memory_write_budget_exhausted",
]


@dataclass(frozen=True)
class MemoryWriteBudget:
    max_writes: int
    writes_used: int = 0


def memory_record_expired(record: MemoryRecord, now_ms: int) -> bool:
    return record.ttl_days is not None and now_ms > record.updated_at + record.ttl_days * 86_400_000


def memory_scope_matches(left: MemoryScope, right: MemoryScope) -> bool:
    return left == right


def verify_memory_lease(lease: MemoryLease | None, agent_id: str, scope: MemoryScope, now_ms: int, budget: MemoryWriteBudget | None = None) -> tuple[bool, MemoryLeaseRejection | None]:
    if lease is None:
        return False, "lease_missing"
    if lease.agent_id != agent_id or not memory_scope_matches(scope, lease.scope):
        return False, "lease_scope_mismatch"
    if lease.revoked_at is not None and now_ms >= lease.revoked_at:
        return False, "lease_revoked"
    if now_ms >= lease.expires_at:
        return False, "lease_expired"
    max_writes = budget.max_writes if budget is not None else lease.max_writes
    writes_used = budget.writes_used if budget is not None else lease.writes_used
    if writes_used >= max_writes:
        return False, "memory_write_budget_exhausted"
    return True, None


def consume_memory_lease_write(lease: MemoryLease) -> MemoryLease:
    return replace(lease, writes_used=lease.writes_used + 1)


def revoke_memory_lease(lease: MemoryLease, revoked_at: int) -> MemoryLease:
    return replace(lease, revoked_at=revoked_at)


def scope_matches_memory_deletion(record: MemoryRecord, *, agent_id: str | None = None, tenant_id: str | None = None, namespace: str | None = None) -> bool:
    return (tenant_id is None or record.scope.tenant_id == tenant_id) and (namespace is None or record.scope.namespace == namespace)
