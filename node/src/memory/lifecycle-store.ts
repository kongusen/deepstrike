import type {
  MemoryDeletionReceipt,
  MemoryLifecycleEvent,
  MemoryLifecycleStore,
  MemoryLifecycleWriteResult,
  MemoryLease,
} from "./lifecycle.js"
import { memoryScopeMatches, consumeMemoryLeaseWrite, revokeMemoryLease, verifyMemoryLease } from "./lifecycle.js"
import type { MemoryScope } from "./protocols.js"

/** In-process lifecycle controller used by embedders and deterministic tests. */
export class InMemoryMemoryLifecycleStore implements MemoryLifecycleStore {
  private leases = new Map<string, MemoryLease>()
  private consumed = new Map<string, MemoryLifecycleWriteResult>()
  private deletedRecords = new Map<string, MemoryDeletionReceipt>()
  private readonly journal: MemoryLifecycleEvent[] = []

  async issueLease(lease: MemoryLease): Promise<MemoryLease> {
    const existing = this.leases.get(lease.lease_id)
    if (existing) {
      if (JSON.stringify(existing) !== JSON.stringify(lease)) throw new Error("memory lease conflict")
      return structuredClone(existing)
    }
    this.leases.set(lease.lease_id, structuredClone(lease))
    this.append("lease_issued", lease, undefined, lease.issued_at, { max_writes: lease.max_writes, expires_at: lease.expires_at })
    return structuredClone(lease)
  }

  async getLease(agentId: string, scope: MemoryScope): Promise<MemoryLease | undefined> {
    const lease = [...this.leases.values()].find(item => item.agent_id === agentId && memoryScopeMatches(item.scope, scope))
    return lease ? structuredClone(lease) : undefined
  }

  async renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): Promise<MemoryLease> {
    const lease = this.requireLease(agentId, leaseId)
    if (expiresAt <= renewedAt || expiresAt < lease.expires_at) throw new Error("invalid memory lease renewal")
    const updated = { ...lease, expires_at: expiresAt }
    this.leases.set(leaseId, updated)
    this.append("lease_renewed", updated, undefined, renewedAt, { expires_at: expiresAt })
    return structuredClone(updated)
  }

  async consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): Promise<MemoryLifecycleWriteResult> {
    const key = `${agentId}:${operationId}`
    const prior = this.consumed.get(key)
    if (prior) return structuredClone(prior)
    const lease = await this.getLease(agentId, scope)
    const verdict = verifyMemoryLease(lease, agentId, scope, nowMs)
    if (!verdict.valid) {
      const rejected: MemoryLifecycleWriteResult = verdict
      this.consumed.set(key, rejected)
      return structuredClone(rejected)
    }
    const updated = consumeMemoryLeaseWrite(lease!)
    this.leases.set(updated.lease_id, updated)
    const event = this.append("lease_write_consumed", updated, operationId, nowMs, { writes_used: updated.writes_used })
    const accepted: MemoryLifecycleWriteResult = { valid: true, lease: updated, event_id: event.event_id }
    this.consumed.set(key, accepted)
    return structuredClone(accepted)
  }

  async deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): Promise<MemoryDeletionReceipt> {
    const key = `${agentId}:${deletionId}`
    const existing = [...this.deletedRecords.values()].find(item => `${item.agent_id}:${item.deletion_id}` === key)
    if (existing) return structuredClone(existing)
    const deletionLease: MemoryLease = {
      lease_id: `deletion:${deletionId}`, agent_id: agentId, scope: structuredClone(scope), issued_at: 0, expires_at: 0, max_writes: 0, writes_used: 0,
    }
    const payload = { deletion_id: deletionId, record_id: recordId, ...(priorDigest ? { prior_digest: priorDigest } : {}) }
    this.append("deletion_requested", deletionLease, deletionId, this.now(), payload)
    const event = this.append("deletion_applied", deletionLease, deletionId, this.now(), payload)
    const receipt: MemoryDeletionReceipt = {
      deletion_id: deletionId, agent_id: agentId, scope: structuredClone(scope), record_id: recordId,
      ...(priorDigest ? { prior_digest: priorDigest } : {}), status: "applied", event_sequence: event.sequence,
    }
    this.deletedRecords.set(key, receipt)
    return structuredClone(receipt)
  }

  async deleted(agentId?: string): Promise<MemoryDeletionReceipt[]> {
    return [...this.deletedRecords.values()].filter(item => agentId === undefined || item.agent_id === agentId).map(item => structuredClone(item))
  }

  async revokeLease(agentId: string, leaseId: string, revokedAt: number): Promise<MemoryLease> {
    const lease = this.requireLease(agentId, leaseId)
    if (lease.revoked_at !== undefined) return structuredClone(lease)
    const updated = revokeMemoryLease(lease, revokedAt)
    this.leases.set(leaseId, updated)
    this.append("lease_revoked", updated, undefined, revokedAt, { revoked_at: revokedAt })
    return structuredClone(updated)
  }

  async events(): Promise<MemoryLifecycleEvent[]> {
    return structuredClone(this.journal)
  }

  private now(): number { return Date.now() }

  private requireLease(agentId: string, leaseId: string): MemoryLease {
    const lease = this.leases.get(leaseId)
    if (!lease || lease.agent_id !== agentId) throw new Error("memory lease not found")
    return lease
  }

  private append(type: MemoryLifecycleEvent["type"], lease: MemoryLease, operationId: string | undefined, occurredAt: number, payload: Record<string, unknown>): MemoryLifecycleEvent {
    const event: MemoryLifecycleEvent = {
      event_id: `${type}:${lease.lease_id}:${operationId ?? this.journal.length}`,
      sequence: this.journal.length,
      type,
      agent_id: lease.agent_id,
      lease_id: lease.lease_id,
      scope: structuredClone(lease.scope),
      ...(operationId ? { operation_id: operationId } : {}),
      occurred_at: occurredAt,
      payload,
    }
    this.journal.push(event)
    return event
  }
}
