import { mkdir, open, readFile, rename, rm, stat, writeFile } from "node:fs/promises"
import { dirname } from "node:path"
import { memoryScopeMatches, consumeMemoryLeaseWrite, revokeMemoryLease, verifyMemoryLease } from "./lifecycle.js"
import type { MemoryDeletionReceipt, MemoryLifecycleEvent, MemoryLifecycleStore, MemoryLifecycleWriteResult, MemoryLease } from "./lifecycle.js"
import type { MemoryScope } from "./protocols.js"

interface LifecycleState {
  version: 1
  leases: MemoryLease[]
  consumed: Array<{ key: string; result: MemoryLifecycleWriteResult }>
  deleted: MemoryDeletionReceipt[]
  events: MemoryLifecycleEvent[]
}

export interface FileMemoryLifecycleStoreOptions {
  path: string
  now?: () => number
  lockRetryMs?: number
  lockTimeoutMs?: number
  deletePhysical?: (agentId: string, scope: MemoryScope, recordId: string) => Promise<boolean>
}

/** Atomic file-backed lease and write-budget controller. */
export class FileMemoryLifecycleStore implements MemoryLifecycleStore {
  private readonly path: string
  private readonly lockPath: string
  private readonly now: () => number
  private readonly lockRetryMs: number
  private readonly lockTimeoutMs: number
  private readonly deletePhysical: (agentId: string, scope: MemoryScope, recordId: string) => Promise<boolean>

  constructor(options: FileMemoryLifecycleStoreOptions | string) {
    const resolved = typeof options === "string" ? { path: options } : options
    this.path = resolved.path
    this.lockPath = `${resolved.path}.lock`
    this.now = resolved.now ?? Date.now
    this.lockRetryMs = resolved.lockRetryMs ?? 10
    this.lockTimeoutMs = resolved.lockTimeoutMs ?? 30_000
    this.deletePhysical = resolved.deletePhysical ?? (async () => true)
  }

  async issueLease(lease: MemoryLease): Promise<MemoryLease> {
    return this.mutate(state => {
      const existing = state.leases.find(item => item.lease_id === lease.lease_id)
      if (existing) {
        if (JSON.stringify(existing) !== JSON.stringify(lease)) throw new Error("memory lease conflict")
        return structuredClone(existing)
      }
      state.leases.push(structuredClone(lease))
      this.append(state, "lease_issued", lease, undefined, lease.issued_at, { max_writes: lease.max_writes, expires_at: lease.expires_at })
      return structuredClone(lease)
    })
  }

  async getLease(agentId: string, scope: MemoryScope): Promise<MemoryLease | undefined> {
    return this.withLock(async () => {
      const state = await this.readState()
      const lease = state.leases.find(item => item.agent_id === agentId && memoryScopeMatches(item.scope, scope))
      return lease ? structuredClone(lease) : undefined
    })
  }

  async renewLease(agentId: string, leaseId: string, expiresAt: number, renewedAt: number): Promise<MemoryLease> {
    return this.mutate(state => {
      const lease = this.requireLease(state, agentId, leaseId)
      if (expiresAt <= renewedAt || expiresAt < lease.expires_at) throw new Error("invalid memory lease renewal")
      const updated = { ...lease, expires_at: expiresAt }
      state.leases[state.leases.indexOf(lease)] = updated
      this.append(state, "lease_renewed", updated, undefined, renewedAt, { expires_at: expiresAt })
      return structuredClone(updated)
    })
  }

  async consumeWrite(agentId: string, scope: MemoryScope, operationId: string, nowMs: number): Promise<MemoryLifecycleWriteResult> {
    return this.mutate(state => {
      const key = `${agentId}:${operationId}`
      const prior = state.consumed.find(item => item.key === key)?.result
      if (prior) return structuredClone(prior)
      const lease = state.leases.find(item => item.agent_id === agentId && memoryScopeMatches(item.scope, scope))
      const verdict = verifyMemoryLease(lease, agentId, scope, nowMs)
      if (!verdict.valid) {
        state.consumed.push({ key, result: verdict })
        return structuredClone(verdict)
      }
      const updated = consumeMemoryLeaseWrite(lease!)
      state.leases[state.leases.indexOf(lease!)] = updated
      const event = this.append(state, "lease_write_consumed", updated, operationId, nowMs, { writes_used: updated.writes_used })
      const result: MemoryLifecycleWriteResult = { valid: true, lease: updated, event_id: event.event_id }
      state.consumed.push({ key, result })
      return structuredClone(result)
    })
  }

  async deleteRecord(agentId: string, scope: MemoryScope, recordId: string, deletionId: string, priorDigest?: string): Promise<MemoryDeletionReceipt> {
    const request = {
      deletion_id: deletionId,
      record_id: recordId,
      ...(priorDigest ? { prior_digest: priorDigest } : {}),
    }
    await this.withLock(async () => {
      const state = await this.readState()
      if (state.deleted.some(item => item.agent_id === agentId && item.deletion_id === deletionId)) return
      const alreadyRequested = state.events.some(event => event.type === "deletion_requested" && event.agent_id === agentId && event.operation_id === deletionId)
      if (!alreadyRequested) {
        this.append(state, "deletion_requested", {
          lease_id: `deletion:${deletionId}`, agent_id: agentId, scope, issued_at: 0, expires_at: 0, max_writes: 0, writes_used: 0,
        }, deletionId, this.now(), request)
        await this.writeState(state)
      }
    })

    const physicallyDeleted = await this.deletePhysical(agentId, scope, recordId)
    return this.withLock(async () => {
      const state = await this.readState()
      const existing = state.deleted.find(item => item.agent_id === agentId && item.deletion_id === deletionId)
      if (existing) return structuredClone(existing)
      const type = physicallyDeleted ? "deletion_applied" : "deletion_noop"
      const event = this.append(state, type, {
        lease_id: `deletion:${deletionId}`, agent_id: agentId, scope, issued_at: 0, expires_at: 0, max_writes: 0, writes_used: 0,
      }, deletionId, this.now(), request)
      const receipt: MemoryDeletionReceipt = {
        deletion_id: deletionId, agent_id: agentId, scope: structuredClone(scope), record_id: recordId,
        ...(priorDigest ? { prior_digest: priorDigest } : {}), status: physicallyDeleted ? "applied" : "noop", event_sequence: event.sequence,
      }
      state.deleted.push(receipt)
      await this.writeState(state)
      return structuredClone(receipt)
    })
  }

  async deleted(agentId?: string): Promise<MemoryDeletionReceipt[]> {
    return this.withLock(async () => (await this.readState()).deleted.filter(item => agentId === undefined || item.agent_id === agentId).map(item => structuredClone(item)))
  }

  async revokeLease(agentId: string, leaseId: string, revokedAt: number): Promise<MemoryLease> {
    return this.mutate(state => {
      const lease = this.requireLease(state, agentId, leaseId)
      if (lease.revoked_at !== undefined) return structuredClone(lease)
      const updated = revokeMemoryLease(lease, revokedAt)
      state.leases[state.leases.indexOf(lease)] = updated
      this.append(state, "lease_revoked", updated, undefined, revokedAt, { revoked_at: revokedAt })
      return structuredClone(updated)
    })
  }

  async events(): Promise<MemoryLifecycleEvent[]> {
    return this.withLock(async () => structuredClone((await this.readState()).events))
  }

  private async mutate<T>(fn: (state: LifecycleState) => T): Promise<T> {
    return this.withLock(async () => {
      const state = await this.readState()
      const result = fn(state)
      await this.writeState(state)
      return result
    })
  }

  private requireLease(state: LifecycleState, agentId: string, leaseId: string): MemoryLease {
    const lease = state.leases.find(item => item.lease_id === leaseId)
    if (!lease || lease.agent_id !== agentId) throw new Error("memory lease not found")
    return lease
  }

  private append(state: LifecycleState, type: MemoryLifecycleEvent["type"], lease: MemoryLease, operationId: string | undefined, occurredAt: number, payload: Record<string, unknown>): MemoryLifecycleEvent {
    const event: MemoryLifecycleEvent = {
      event_id: `${type}:${lease.lease_id}:${operationId ?? state.events.length}`,
      sequence: state.events.length,
      type,
      agent_id: lease.agent_id,
      lease_id: lease.lease_id,
      scope: structuredClone(lease.scope),
      ...(operationId ? { operation_id: operationId } : {}),
      occurred_at: occurredAt,
      payload,
    }
    state.events.push(event)
    return event
  }

  private async readState(): Promise<LifecycleState> {
    try {
      const raw = JSON.parse(await readFile(this.path, "utf8")) as Partial<LifecycleState>
      if (raw.version !== 1 || !Array.isArray(raw.leases) || !Array.isArray(raw.consumed) || !Array.isArray(raw.deleted) || !Array.isArray(raw.events)) throw new Error("invalid memory lifecycle snapshot")
      return raw as LifecycleState
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return { version: 1, leases: [], consumed: [], deleted: [], events: [] }
      throw new Error(`invalid memory lifecycle snapshot: ${(error as Error).message}`)
    }
  }

  private async writeState(state: LifecycleState): Promise<void> {
    await mkdir(dirname(this.path), { recursive: true })
    const tempPath = `${this.path}.tmp-${process.pid}-${Math.random().toString(16).slice(2)}`
    const handle = await open(tempPath, "w", 0o600)
    try {
      await handle.writeFile(JSON.stringify(state))
      await handle.sync()
    } finally {
      await handle.close()
    }
    await rename(tempPath, this.path)
    try {
      const directory = await open(dirname(this.path), "r")
      await directory.sync()
      await directory.close()
    } catch { /* directory fsync is not available on every host */ }
  }

  private async withLock<T>(fn: () => Promise<T>): Promise<T> {
    const started = this.now()
    while (true) {
      try {
        await mkdir(this.lockPath)
        try { return await fn() } finally { await rm(this.lockPath, { recursive: true, force: true }) }
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST" || this.now() - started >= this.lockTimeoutMs) throw error
        try {
          const info = await stat(this.lockPath)
          if (this.now() - info.mtimeMs >= this.lockTimeoutMs) await rm(this.lockPath, { recursive: true, force: true })
        } catch { /* another writer released it */ }
        await new Promise(resolve => setTimeout(resolve, this.lockRetryMs))
      }
    }
  }
}
