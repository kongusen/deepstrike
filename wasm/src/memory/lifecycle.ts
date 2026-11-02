import type { MemoryRecord, MemoryScope } from "./index.js"

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

export type MemoryLeaseRejection = "lease_missing" | "lease_scope_mismatch" | "lease_expired" | "lease_revoked" | "memory_write_budget_exhausted"
export interface MemoryWriteBudget { max_writes: number; writes_used?: number }
export type MemoryLifecycleEventType = "lease_issued" | "lease_renewed" | "lease_write_consumed" | "lease_revoked" | "deletion_requested" | "deletion_applied" | "deletion_noop"
export interface MemoryLifecycleEvent { event_id: string; sequence: number; type: MemoryLifecycleEventType; agent_id: string; lease_id: string; scope: MemoryScope; operation_id?: string; occurred_at: number; payload: Record<string, unknown> }
export interface MemoryDeletionReceipt { deletion_id: string; agent_id: string; scope: MemoryScope; record_id: string; prior_digest?: string; status: "applied" | "noop"; event_sequence: number }
export type MemoryLifecycleWriteResult = { valid: true; lease: MemoryLease; event_id: string } | { valid: false; reason_code: MemoryLeaseRejection }

export interface MemoryLifecycleController {
  issueLease(lease: MemoryLease): MemoryLease
  getLease(agentId: string, scope: MemoryScope): MemoryLease | undefined
  renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): MemoryLease
  consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): MemoryLifecycleWriteResult
  revokeLease(agentId: string, leaseId: string, revokedAt: number): MemoryLease
  deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): MemoryDeletionReceipt
  events(): MemoryLifecycleEvent[]
}

export function memoryRecordExpired(record: Pick<MemoryRecord, "updated_at" | "ttl_days">, nowMs: number): boolean {
  return record.ttl_days !== undefined && nowMs > record.updated_at + record.ttl_days * 86_400_000
}

export function verifyMemoryLease(lease: MemoryLease | undefined, agentId: string, scope: MemoryScope, nowMs: number, budget?: MemoryWriteBudget): { valid: true } | { valid: false; reason_code: MemoryLeaseRejection } {
  if (!lease) return { valid: false, reason_code: "lease_missing" }
  if (lease.agent_id !== agentId || lease.scope.tenant_id !== scope.tenant_id || lease.scope.namespace !== scope.namespace) return { valid: false, reason_code: "lease_scope_mismatch" }
  if (lease.revoked_at !== undefined && nowMs >= lease.revoked_at) return { valid: false, reason_code: "lease_revoked" }
  if (nowMs >= lease.expires_at) return { valid: false, reason_code: "lease_expired" }
  if ((budget?.writes_used ?? lease.writes_used) >= (budget?.max_writes ?? lease.max_writes)) return { valid: false, reason_code: "memory_write_budget_exhausted" }
  return { valid: true }
}

export function consumeMemoryLeaseWrite(lease: MemoryLease): MemoryLease { return { ...lease, writes_used: lease.writes_used + 1 } }
export function revokeMemoryLease(lease: MemoryLease, revokedAt: number): MemoryLease { return { ...lease, revoked_at: revokedAt } }
export function scopeMatchesMemoryDeletion(record: MemoryRecord, scope: { tenant_id?: string; namespace?: string }): boolean {
  return (scope.tenant_id === undefined || record.scope.tenant_id === scope.tenant_id) && (scope.namespace === undefined || record.scope.namespace === scope.namespace)
}

export class InMemoryMemoryLifecycleController implements MemoryLifecycleController {
  private leases = new Map<string, MemoryLease>()
  private consumed = new Map<string, MemoryLifecycleWriteResult>()
  private deletedRecords = new Map<string, MemoryDeletionReceipt>()
  private journal: MemoryLifecycleEvent[] = []
  issueLease(lease: MemoryLease): MemoryLease { const old = this.leases.get(lease.lease_id); if (old && JSON.stringify(old) !== JSON.stringify(lease)) throw new Error("memory lease conflict"); if (old) return structuredClone(old); this.leases.set(lease.lease_id, structuredClone(lease)); this.append("lease_issued", lease, undefined, lease.issued_at, { max_writes: lease.max_writes, expires_at: lease.expires_at }); return structuredClone(lease) }
  getLease(agentId: string, scope: MemoryScope): MemoryLease | undefined { const lease = [...this.leases.values()].find(item => item.agent_id === agentId && item.scope.tenant_id === scope.tenant_id && item.scope.namespace === scope.namespace); return lease ? structuredClone(lease) : undefined }
  renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): MemoryLease { const lease = this.require(agentId, leaseId); if (expiresAt <= renewedAt || expiresAt < lease.expires_at) throw new Error("invalid memory lease renewal"); const updated = { ...lease, expires_at: expiresAt }; this.leases.set(leaseId, updated); this.append("lease_renewed", updated, undefined, renewedAt, { expires_at: expiresAt }); return structuredClone(updated) }
  consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): MemoryLifecycleWriteResult { const key = `${agentId}:${operationId}`; const prior = this.consumed.get(key); if (prior) return structuredClone(prior); const lease = this.getLease(agentId, scope); const verdict = verifyMemoryLease(lease, agentId, scope, nowMs); if (!verdict.valid) { this.consumed.set(key, verdict); return structuredClone(verdict) } const updated = consumeMemoryLeaseWrite(lease!); this.leases.set(updated.lease_id, updated); const event = this.append("lease_write_consumed", updated, operationId, nowMs, { writes_used: updated.writes_used }); const result = { valid: true as const, lease: updated, event_id: event.event_id }; this.consumed.set(key, result); return structuredClone(result) }
  revokeLease(agentId: string, leaseId: string, revokedAt: number): MemoryLease { const lease = this.require(agentId, leaseId); if (lease.revoked_at !== undefined) return structuredClone(lease); const updated = revokeMemoryLease(lease, revokedAt); this.leases.set(leaseId, updated); this.append("lease_revoked", updated, undefined, revokedAt, { revoked_at: revokedAt }); return structuredClone(updated) }
  deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): MemoryDeletionReceipt { const key = `${agentId}:${deletionId}`; const old = this.deletedRecords.get(key); if (old) return structuredClone(old); const payload = { deletion_id: deletionId, record_id: recordId, ...(priorDigest ? { prior_digest: priorDigest } : {}) }; const lease = { lease_id: `deletion:${deletionId}`, agent_id: agentId, scope: structuredClone(scope), issued_at: 0, expires_at: 0, max_writes: 0, writes_used: 0 }; this.append("deletion_requested", lease, deletionId, Date.now(), payload); const event = this.append("deletion_applied", lease, deletionId, Date.now(), payload); const receipt = { deletion_id: deletionId, agent_id: agentId, scope: structuredClone(scope), record_id: recordId, ...(priorDigest ? { prior_digest: priorDigest } : {}), status: "applied" as const, event_sequence: event.sequence }; this.deletedRecords.set(key, receipt); return structuredClone(receipt) }
  events(): MemoryLifecycleEvent[] { return structuredClone(this.journal) }
  private require(agentId: string, leaseId: string): MemoryLease { const lease = this.leases.get(leaseId); if (!lease || lease.agent_id !== agentId) throw new Error("memory lease not found"); return lease }
  private append(type: MemoryLifecycleEventType, lease: MemoryLease, operationId: string | undefined, occurredAt: number, payload: Record<string, unknown>): MemoryLifecycleEvent { const event = { event_id: `${type}:${lease.lease_id}:${operationId ?? this.journal.length}`, sequence: this.journal.length, type, agent_id: lease.agent_id, lease_id: lease.lease_id, scope: structuredClone(lease.scope), ...(operationId ? { operation_id: operationId } : {}), occurred_at: occurredAt, payload }; this.journal.push(event); return event }
}
