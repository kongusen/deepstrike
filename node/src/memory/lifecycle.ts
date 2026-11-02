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

export type MemoryLifecycleEventType =
  | "lease_issued"
  | "lease_renewed"
  | "lease_write_consumed"
  | "lease_revoked"
  | "deletion_requested"
  | "deletion_applied"
  | "deletion_noop"

export interface MemoryDeletionScope {
  agent_id: string
  scope: MemoryScope
  record_id: string
}

export interface MemoryDeletionReceipt {
  deletion_id: string
  agent_id: string
  scope: MemoryScope
  record_id: string
  prior_digest?: string
  status: "applied" | "noop"
  event_sequence: number
}

export interface MemoryLifecycleReplayState {
  leases: MemoryLease[]
  deleted: MemoryDeletionReceipt[]
  events: MemoryLifecycleEvent[]
}

export interface MemoryLifecycleEvent {
  event_id: string
  sequence: number
  type: MemoryLifecycleEventType
  agent_id: string
  lease_id: string
  scope: MemoryScope
  operation_id?: string
  occurred_at: number
  payload: Record<string, unknown>
}

export type MemoryLifecycleWriteResult = {
  valid: true
  lease: MemoryLease
  event_id: string
} | {
  valid: false
  reason_code: MemoryLeaseRejection
}

export interface MemoryLifecycleStore {
  issueLease(lease: MemoryLease): Promise<MemoryLease>
  getLease(agentId: string, scope: MemoryScope): Promise<MemoryLease | undefined>
  renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): Promise<MemoryLease>
  consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): Promise<MemoryLifecycleWriteResult>
  revokeLease(agentId: string, leaseId: string, revokedAt: number): Promise<MemoryLease>
  deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): Promise<MemoryDeletionReceipt>
  deleted(agentId?: string): Promise<MemoryDeletionReceipt[]>
  events(): Promise<MemoryLifecycleEvent[]>
}

export function replayMemoryLifecycle(events: readonly MemoryLifecycleEvent[]): MemoryLifecycleReplayState {
  const leases = new Map<string, MemoryLease>()
  const deleted = new Map<string, MemoryDeletionReceipt>()
  let sequence = 0
  const seen = new Map<string, string>()
  for (const event of events) {
    const serialized = JSON.stringify(event)
    const prior = seen.get(event.event_id)
    if (prior !== undefined && prior !== serialized) throw new Error("memory lifecycle event conflict")
    if (prior !== undefined) continue
    seen.set(event.event_id, serialized)
    if (event.sequence !== sequence) throw new Error("memory lifecycle sequence gap")
    sequence += 1
    if (event.type === "lease_issued") {
      leases.set(event.lease_id, { lease_id: event.lease_id, agent_id: event.agent_id, scope: structuredClone(event.scope), issued_at: event.occurred_at, expires_at: Number(event.payload.expires_at), max_writes: Number(event.payload.max_writes), writes_used: 0 })
    } else if (event.type === "lease_renewed") {
      const lease = leases.get(event.lease_id); if (!lease) throw new Error("memory lifecycle lease missing")
      lease.expires_at = Number(event.payload.expires_at)
    } else if (event.type === "lease_write_consumed") {
      const lease = leases.get(event.lease_id); if (!lease) throw new Error("memory lifecycle lease missing")
      lease.writes_used = Number(event.payload.writes_used)
    } else if (event.type === "lease_revoked") {
      const lease = leases.get(event.lease_id); if (!lease) throw new Error("memory lifecycle lease missing")
      lease.revoked_at = Number(event.payload.revoked_at)
    } else if (event.type === "deletion_applied" || event.type === "deletion_noop") {
      const payload = event.payload
      deleted.set(String(payload.deletion_id), { deletion_id: String(payload.deletion_id), agent_id: event.agent_id, scope: structuredClone(event.scope), record_id: String(payload.record_id), ...(payload.prior_digest ? { prior_digest: String(payload.prior_digest) } : {}), status: event.type === "deletion_applied" ? "applied" : "noop", event_sequence: event.sequence })
    }
  }
  return { leases: [...leases.values()], deleted: [...deleted.values()], events: structuredClone([...events]) }
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
