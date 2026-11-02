import { mkdir, open, readFile, rename, rm, stat, writeFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { randomUUID } from "node:crypto"

import {
  memoryCandidateDuplicateReason,
  memoryCandidatePayloadDigest,
  type MemoryCandidate,
  type MemoryCandidateReceipt,
  type MemoryCandidateSink,
} from "./candidate-sink.js"
import { admitMemoryCandidate, memoryCandidateAttestationDigest, type MemoryCandidateSourceVerifier } from "./attestation.js"

export type MemoryCandidateResolution =
  | { status: "accepted"; record_ref: string; digest: string }
  | { status: "rejected"; reason_code: string }

export interface MemoryCandidateLedgerEntry {
  agent_id: string
  candidate: MemoryCandidate
  payload_digest: string
  receipt: MemoryCandidateReceipt
  created_at: number
  updated_at: number
}

export interface MemoryCandidateOutboxEvent {
  event_id: string
  type: "candidate_submitted" | "candidate_accepted" | "candidate_rejected"
  agent_id: string
  candidate_id: string
  receipt: MemoryCandidateReceipt
  occurred_at: number
}

export interface FileMemoryCandidateSinkOptions {
  path: string
  now?: () => number
  lockRetryMs?: number
  lockTimeoutMs?: number
  sourceVerifier?: MemoryCandidateSourceVerifier
}

export interface MemoryCandidateVerificationAttempt {
  attempt_id: string
  agent_id: string
  candidate_id: string
  reason_code: string
  attestation_digest?: string
  issuer?: string
  key_id?: string
  occurred_at: number
}

interface LedgerState {
  version: 1
  next_receipt: number
  next_event: number
  entries: MemoryCandidateLedgerEntry[]
  outbox: MemoryCandidateOutboxEvent[]
  verification_attempts: MemoryCandidateVerificationAttempt[]
}

/**
 * Atomic file-backed candidate ledger for hosts that do not yet have a database sink.
 *
 * The snapshot contains candidates and their outbox in one commit. A lock directory serializes
 * writers across processes; a temp-file/fsync/rename sequence makes each commit recoverable after
 * a crash. Corrupt snapshots are rejected rather than silently replaced with empty state.
 */
export class FileMemoryCandidateSink implements MemoryCandidateSink {
  private readonly path: string
  private readonly lockPath: string
  private readonly now: () => number
  private readonly lockRetryMs: number
  private readonly lockTimeoutMs: number
  private readonly sourceVerifier?: MemoryCandidateSourceVerifier

  constructor(options: FileMemoryCandidateSinkOptions | string) {
    const resolved = typeof options === "string" ? { path: options } : options
    this.path = resolved.path
    this.lockPath = `${resolved.path}.lock`
    this.now = resolved.now ?? (() => Date.now())
    this.lockRetryMs = resolved.lockRetryMs ?? 10
    this.lockTimeoutMs = resolved.lockTimeoutMs ?? 30_000
    this.sourceVerifier = resolved.sourceVerifier
  }

  async submit(agentId: string, candidate: MemoryCandidate): Promise<MemoryCandidateReceipt> {
    return this.withLock(async () => {
      const state = await this.readState()
      const payloadDigest = memoryCandidatePayloadDigest(candidate.record)
      const existing = state.entries.find(entry =>
        entry.agent_id === agentId && entry.candidate.candidate_id === candidate.candidate_id)
      if (existing) {
        if (existing.payload_digest === payloadDigest && existing.candidate.source === candidate.source) {
          return structuredClone(existing.receipt)
        }
        const receipt: MemoryCandidateReceipt = {
          status: "rejected",
          candidate_id: candidate.candidate_id,
          receipt_id: this.mintReceipt(state),
          reason_code: "candidate_conflict",
        }
        this.appendOutbox(state, "candidate_rejected", agentId, candidate.candidate_id, receipt)
        await this.writeState(state)
        return structuredClone(receipt)
      }

      const verification = admitMemoryCandidate(agentId, candidate, this.sourceVerifier)
      if (!verification.valid) {
        const receipt: MemoryCandidateReceipt = {
          status: "rejected",
          candidate_id: candidate.candidate_id,
          receipt_id: this.mintReceipt(state),
          reason_code: verification.reason_code,
        }
        state.verification_attempts.push({
          attempt_id: `verification:${state.next_event}`,
          agent_id: agentId,
          candidate_id: candidate.candidate_id,
          reason_code: verification.reason_code,
          ...(candidate.attestation ? {
            attestation_digest: memoryCandidateAttestationDigest(candidate.attestation),
            issuer: candidate.attestation.issuer,
            key_id: candidate.attestation.key_id,
          } : {}),
          occurred_at: this.now(),
        })
        state.next_event += 1
        await this.writeState(state)
        return structuredClone(receipt)
      }

      const duplicate = memoryCandidateDuplicateReason(state.entries.find(entry =>
        entry.agent_id === agentId && entry.payload_digest === payloadDigest && entry.receipt.status !== "rejected")?.receipt.status)
      if (duplicate) {
        const receipt: MemoryCandidateReceipt = {
          status: "rejected",
          candidate_id: candidate.candidate_id,
          receipt_id: this.mintReceipt(state),
          reason_code: duplicate,
        }
        this.appendOutbox(state, "candidate_rejected", agentId, candidate.candidate_id, receipt)
        await this.writeState(state)
        return structuredClone(receipt)
      }

      const timestamp = this.now()
      const receipt: MemoryCandidateReceipt = {
        status: "pending",
        candidate_id: candidate.candidate_id,
        receipt_id: this.mintReceipt(state),
      }
      state.entries.push({
        agent_id: agentId,
        candidate: structuredClone(candidate),
        payload_digest: payloadDigest,
        receipt: structuredClone(receipt),
        created_at: timestamp,
        updated_at: timestamp,
      })
      this.appendOutbox(state, "candidate_submitted", agentId, candidate.candidate_id, receipt)
      await this.writeState(state)
      return structuredClone(receipt)
    })
  }

  async verificationAttempts(agentId?: string): Promise<MemoryCandidateVerificationAttempt[]> {
    return this.withLock(async () => (await this.readState()).verification_attempts
      .filter(attempt => agentId === undefined || attempt.agent_id === agentId)
      .map(attempt => structuredClone(attempt)))
  }

  async list(agentId?: string): Promise<MemoryCandidateLedgerEntry[]> {
    return this.withLock(async () => (await this.readState()).entries
      .filter(entry => agentId === undefined || entry.agent_id === agentId)
      .map(entry => structuredClone(entry)))
  }

  async resolve(agentId: string, candidateId: string, resolution: MemoryCandidateResolution): Promise<MemoryCandidateReceipt> {
    return this.withLock(async () => {
      const state = await this.readState()
      const entry = state.entries.find(item =>
        item.agent_id === agentId && item.candidate.candidate_id === candidateId)
      if (!entry) throw new Error(`unknown memory candidate: ${agentId}/${candidateId}`)
      if (entry.receipt.status !== "pending") return structuredClone(entry.receipt)

      const receipt: MemoryCandidateReceipt = resolution.status === "accepted"
        ? {
            status: "accepted",
            candidate_id: candidateId,
            receipt_id: entry.receipt.receipt_id,
            record_ref: resolution.record_ref,
            digest: resolution.digest,
          }
        : {
            status: "rejected",
            candidate_id: candidateId,
            receipt_id: entry.receipt.receipt_id,
            reason_code: resolution.reason_code,
          }
      entry.receipt = receipt
      entry.updated_at = this.now()
      this.appendOutbox(state, resolution.status === "accepted" ? "candidate_accepted" : "candidate_rejected", agentId, candidateId, receipt)
      await this.writeState(state)
      return structuredClone(receipt)
    })
  }

  async outbox(agentId?: string): Promise<MemoryCandidateOutboxEvent[]> {
    return this.withLock(async () => (await this.readState()).outbox
      .filter(event => agentId === undefined || event.agent_id === agentId)
      .map(event => structuredClone(event)))
  }

  async ackOutbox(eventId: string): Promise<boolean> {
    return this.withLock(async () => {
      const state = await this.readState()
      const before = state.outbox.length
      state.outbox = state.outbox.filter(event => event.event_id !== eventId)
      if (state.outbox.length === before) return false
      await this.writeState(state)
      return true
    })
  }

  private mintReceipt(state: LedgerState): string {
    const receipt = `receipt:${state.next_receipt}`
    state.next_receipt += 1
    return receipt
  }

  private appendOutbox(
    state: LedgerState,
    type: MemoryCandidateOutboxEvent["type"],
    agentId: string,
    candidateId: string,
    receipt: MemoryCandidateReceipt,
  ): void {
    state.outbox.push({
      event_id: `outbox:${state.next_event}`,
      type,
      agent_id: agentId,
      candidate_id: candidateId,
      receipt: structuredClone(receipt),
      occurred_at: this.now(),
    })
    state.next_event += 1
  }

  private async readState(): Promise<LedgerState> {
    try {
      const parsed = JSON.parse(await readFile(this.path, "utf8")) as Partial<LedgerState>
      const nextReceipt = parsed.next_receipt
      const nextEvent = parsed.next_event
      if (parsed.version !== 1 || typeof nextReceipt !== "number" || !Number.isInteger(nextReceipt) || nextReceipt < 1
        || typeof nextEvent !== "number" || !Number.isInteger(nextEvent) || nextEvent < 1
        || !Array.isArray(parsed.entries) || !Array.isArray(parsed.outbox) || !Array.isArray((parsed as { verification_attempts?: unknown }).verification_attempts ?? [])) {
        throw new Error(`invalid memory candidate ledger: ${this.path}`)
      }
      return { version: 1, next_receipt: nextReceipt as number, next_event: nextEvent as number, entries: parsed.entries as MemoryCandidateLedgerEntry[], outbox: parsed.outbox as MemoryCandidateOutboxEvent[], verification_attempts: ((parsed as { verification_attempts?: MemoryCandidateVerificationAttempt[] }).verification_attempts ?? []) }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") {
        return { version: 1, next_receipt: 1, next_event: 1, entries: [], outbox: [], verification_attempts: [] }
      }
      throw error
    }
  }

  private async writeState(state: LedgerState): Promise<void> {
    await mkdir(dirname(this.path), { recursive: true })
    const temporary = `${this.path}.${process.pid}.${randomUUID()}.tmp`
    let descriptor: Awaited<ReturnType<typeof open>> | undefined
    try {
      descriptor = await open(temporary, "w", 0o600)
      await descriptor.writeFile(JSON.stringify(state, null, 2), "utf8")
      await descriptor.sync()
      await descriptor.close()
      descriptor = undefined
      await rename(temporary, this.path)
      try {
        const directory = await open(dirname(this.path), "r")
        await directory.sync()
        await directory.close()
      } catch {
        // Directory fsync is unavailable on some hosts; the file rename remains atomic.
      }
    } finally {
      await descriptor?.close().catch(() => undefined)
      await rm(temporary, { force: true })
    }
  }

  private async withLock<T>(operation: () => Promise<T>): Promise<T> {
    await mkdir(dirname(this.path), { recursive: true })
    const started = Date.now()
    while (true) {
      try {
        await mkdir(this.lockPath)
        break
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error
        try {
          const age = Date.now() - (await stat(this.lockPath)).mtimeMs
          if (age > this.lockTimeoutMs) await rm(this.lockPath, { recursive: true, force: true })
        } catch (statError) {
          if ((statError as NodeJS.ErrnoException).code !== "ENOENT") throw statError
        }
        if (Date.now() - started >= this.lockTimeoutMs) throw new Error(`timed out acquiring memory candidate ledger lock: ${this.lockPath}`)
        await new Promise(resolve => setTimeout(resolve, this.lockRetryMs))
      }
    }
    try {
      return await operation()
    } finally {
      await rm(this.lockPath, { recursive: true, force: true })
    }
  }
}
