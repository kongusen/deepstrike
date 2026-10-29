export interface MemoryLease {
  lease_id: string
  agent_id: string
  scope: { tenant_id: string; namespace: string }
  issued_at: number
  expires_at: number
  max_writes: number
  writes_used: number
  revoked_at?: number
}

export type MemoryLeaseRejection = "lease_missing" | "lease_scope_mismatch" | "lease_expired" | "lease_revoked" | "memory_write_budget_exhausted"

export function verifyMemoryLease(lease: MemoryLease | undefined, agentId: string, scope: { tenant_id: string; namespace: string }, nowMs: number): { valid: true } | { valid: false; reason_code: MemoryLeaseRejection } {
  if (!lease) return { valid: false, reason_code: "lease_missing" }
  if (lease.agent_id !== agentId || lease.scope.tenant_id !== scope.tenant_id || lease.scope.namespace !== scope.namespace) return { valid: false, reason_code: "lease_scope_mismatch" }
  if (lease.revoked_at !== undefined && nowMs >= lease.revoked_at) return { valid: false, reason_code: "lease_revoked" }
  if (nowMs >= lease.expires_at) return { valid: false, reason_code: "lease_expired" }
  if (lease.writes_used >= lease.max_writes) return { valid: false, reason_code: "memory_write_budget_exhausted" }
  return { valid: true }
}

export function memoryRecordExpired(record: { updated_at: number; ttl_days?: number }, nowMs: number): boolean {
  return record.ttl_days !== undefined && nowMs > record.updated_at + record.ttl_days * 86_400_000
}
