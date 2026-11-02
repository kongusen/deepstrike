// The host's side of a memory write, in one place. The runner decides which path a write takes
// (the store, or the candidate sink for review); the writer decides what the write's answer
// names: the stored record and its digest, or the sink's receipt under its contract.

import { sha256Hex } from "../runtime/sha256.js"

import type { MemoryCandidate, MemoryCandidateReceipt, MemoryCandidateSink } from "./candidate-sink.js"
import type { MemoryRecord, MemoryStore } from "./index.js"

export interface MemoryWriterOptions {
  store?: MemoryStore
  sink?: MemoryCandidateSink
}

/** What a direct write settles with: the record the store now holds and its content digest. */
export interface MemoryPersistReceipt {
  record_ref: string
  digest: string
}

export class MemoryWriter {
  constructor(private readonly opts: MemoryWriterOptions) {}

  /** Write `record` to the store. Throws when there is no store. */
  async persist(agentId: string, record: MemoryRecord): Promise<MemoryPersistReceipt> {
    if (!this.opts.store) throw new Error("memory persistence requires RuntimeOptions.memoryStore")
    await this.opts.store.put(agentId, record)
    return { record_ref: record.record_id, digest: sha256Hex(record.content) }
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
