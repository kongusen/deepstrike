// The host's side of a memory write, in one place. The runner decides which path a write takes
// (the store, or the candidate sink for review); the writer decides whether the write may happen
// and what its answer names: lease admission and the store for a direct write, the sink and its
// receipt contract for a reviewed one.

import { createHash } from "node:crypto"

import type { MemoryCandidate, MemoryCandidateReceipt, MemoryCandidateSink } from "./candidate-sink.js"
import type { MemoryLifecycleStore } from "./lifecycle.js"
import type { MemoryRecord, MemoryStore } from "./protocols.js"

export interface MemoryWriterOptions {
  store?: MemoryStore
  /** Only direct store writes consume a lease; the sink's review decides what a candidate costs. */
  lifecycle?: MemoryLifecycleStore
  sink?: MemoryCandidateSink
  now?: () => number
}

/** What a direct write settles with: the record the store now holds and its content digest. */
export interface MemoryPersistReceipt {
  record_ref: string
  digest: string
}

export class MemoryWriter {
  constructor(private readonly opts: MemoryWriterOptions) {}

  /**
   * Write `record` to the store. `writeId` names the write to the lease, so a redelivered write
   * spends the budget once. Throws when there is no store or the lease refuses the write.
   */
  async persist(agentId: string, record: MemoryRecord, writeId: string): Promise<MemoryPersistReceipt> {
    if (!this.opts.store) throw new Error("memory persistence requires memoryStore")
    if (this.opts.lifecycle) {
      const now = this.opts.now?.() ?? Date.now()
      const admission = await this.opts.lifecycle.consumeWrite(agentId, record.scope, writeId, now)
      if (!admission.valid) throw new Error(`memory lease rejected: ${admission.reason_code}`)
    }
    await this.opts.store.put(agentId, record)
    return {
      record_ref: record.record_id,
      digest: `sha256:${createHash("sha256").update(record.content).digest("hex")}`,
    }
  }

  /**
   * Hand `candidate` to the sink and check its answer names this candidate (spec §2.3). A sink that
   * throws has given no answer, and the same candidate may be redelivered.
   */
  async propose(agentId: string, candidate: MemoryCandidate): Promise<MemoryCandidateReceipt> {
    const sink = this.opts.sink
    if (!sink) throw new Error("memory candidate writes require RuntimeOptions.memoryCandidateSink")
    const receipt = await sink.submit(agentId, candidate)
    const candidateId = candidate.candidate_id
    if (receipt.candidate_id !== candidateId || typeof receipt.receipt_id !== "string" || !receipt.receipt_id) {
      throw new Error(`memory candidate sink answered for ${String(receipt.candidate_id)} without a receipt for ${candidateId}`)
    }
    if (receipt.status === "accepted" && (typeof receipt.record_ref !== "string" || !receipt.record_ref || typeof receipt.digest !== "string" || !receipt.digest)) {
      throw new Error("memory candidate sink accepted a write without record_ref/digest")
    }
    if (!["pending", "accepted", "rejected"].includes(receipt.status)) {
      throw new Error(`memory candidate sink answered with unknown status ${String(receipt.status)}`)
    }
    if (receipt.status === "rejected" && (typeof receipt.reason_code !== "string" || !receipt.reason_code)) {
      throw new Error("memory candidate sink rejected a write without reason_code")
    }
    return receipt
  }
}
