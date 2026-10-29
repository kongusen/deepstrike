import type { MemoryRecord, MemoryScope } from "./protocols.js"

export interface MemoryLease {
  lease_id: string
  agent_id: string
  scope: MemoryScope
  issued_at: number
  expires_at: number
  max_writes: number
  writes_used: number
  revoked_at?: number
}

export type MemoryLeaseRejection =
  | "lease_missing"
  | "lease_scope_mismatch"
  | "lease_expired"
  | "lease_revoked"
  | "memory_write_budget_exhausted"

export interface MemoryWriteBudget {
  max_writes: number
  writes_used?: number
}

export function memoryRecordExpired(record: MemoryRecord, nowMs: number): boolean {
  return record.ttl_days !== undefined && nowMs > record.updated_at + record.ttl_days * 86_400_000
}

export function memoryScopeMatches(left: MemoryScope, right: MemoryScope): boolean {
  return left.tenant_id === right.tenant_id && left.namespace === right.namespace
}

export function verifyMemoryLease(lease: MemoryLease | undefined, agentId: string, scope: MemoryScope, nowMs: number, budget?: MemoryWriteBudget): { valid: true } | { valid: false; reason_code: MemoryLeaseRejection } {
  if (!lease) return { valid: false, reason_code: "lease_missing" }
  if (lease.agent_id !== agentId || !memoryScopeMatches(scope, lease.scope)) return { valid: false, reason_code: "lease_scope_mismatch" }
  if (lease.revoked_at !== undefined && nowMs >= lease.revoked_at) return { valid: false, reason_code: "lease_revoked" }
  if (nowMs >= lease.expires_at) return { valid: false, reason_code: "lease_expired" }
  const maxWrites = budget?.max_writes ?? lease.max_writes
  const writesUsed = budget?.writes_used ?? lease.writes_used
  if (writesUsed >= maxWrites) return { valid: false, reason_code: "memory_write_budget_exhausted" }
  return { valid: true }
}

export function consumeMemoryLeaseWrite(lease: MemoryLease): MemoryLease {
  return { ...lease, writes_used: lease.writes_used + 1 }
}

export function revokeMemoryLease(lease: MemoryLease, revokedAt: number): MemoryLease {
  return { ...lease, revoked_at: revokedAt }
}

export function scopeMatchesMemoryDeletion(record: MemoryRecord, scope: { agent_id?: string; tenant_id?: string; namespace?: string }): boolean {
  return (scope.tenant_id === undefined || record.scope.tenant_id === scope.tenant_id)
    && (scope.namespace === undefined || record.scope.namespace === scope.namespace)
}
