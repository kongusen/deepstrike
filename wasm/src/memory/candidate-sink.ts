// Memory candidate hand-off (contract M2). A write path whose `memoryRuntimePolicy` value is
// `"candidate_sink"` does not put the record in the memory store: it hands the record to the
// host's sink as a candidate, and the sink answers whether it is pending review, accepted, or
// rejected. Only an accepted candidate is a remembered record.

import { sha256Hex } from "../runtime/sha256.js"

import type { MemoryRecord } from "./index.js"
import { admitMemoryCandidate, type MemoryCandidateAttestation, type MemoryCandidateSourceVerifier } from "./attestation.js"

/** Which write path produced the candidate. `child_request` is the kernel's PersistMemory effect;
 *  `model_proposal` is the model's own `propose_memory` call. */
export type MemoryCandidateSource = "child_request" | "model_proposal"

export interface MemoryCandidate {
  /** Deterministic for the write that produced it (see {@link memoryCandidateId}): a retry of the
   *  same write carries the same id, so the sink can answer it from what it already holds. */
  candidate_id: string
  source: MemoryCandidateSource
  record: MemoryRecord
  attestation?: MemoryCandidateAttestation
}

export type MemoryCandidateReceipt =
  | { status: "pending"; candidate_id: string; receipt_id: string }
  | { status: "accepted"; candidate_id: string; receipt_id: string; record_ref: string; digest: string }
  | { status: "rejected"; candidate_id: string; receipt_id: string; reason_code: string }

/**
 * The host's candidate store. Contract (spec §2.3):
 *
 * - The sink mints `receipt_id`; it is the stable name of its answer.
 * - Identity is scoped by agentId; source and review metadata are part of conflict detection.
 * - The same `candidate_id` with the same payload returns the original receipt and stores nothing
 *   new, so a write whose acknowledgement was lost can be redelivered safely.
 * - The same `candidate_id` with a different payload is rejected with
 *   `reason_code = "candidate_conflict"` and does not overwrite what is held.
 * - Throwing means no answer was obtained; the caller may redeliver the same candidate.
 */
export interface MemoryCandidateSink {
  submit(agentId: string, candidate: MemoryCandidate): Promise<MemoryCandidateReceipt>
}

/** A candidate id from the write's canonical identity — never random, so a retry repeats it. */
export function memoryCandidateId(parts: readonly string[]): string {
  return sha256Hex(JSON.stringify(parts)).replace(/^sha256:/, "candidate:")
}

/** The part of a record a candidate is judged by. Store ids, timestamps and recall counters are excluded: a retry
 *  of the same write may mint them afresh without changing what is being proposed. */
export function memoryCandidatePayloadDigest(record: MemoryRecord): string {
  const payload = JSON.stringify([
    record.scope.tenant_id,
    record.scope.namespace,
    record.kind,
    record.name,
    record.description,
    record.content,
    record.provenance.author,
    record.provenance.trust,
    record.provenance.session_id ?? null,
    record.provenance.evidence_refs,
    record.confidence,
    record.links,
    record.pinned,
    record.ttl_days ?? null,
  ])
  return sha256Hex(payload)
}

interface HeldCandidate {
  agentId: string
  candidate: MemoryCandidate
  payloadDigest: string
  receipt: MemoryCandidateReceipt
}

/** In-process sink: every candidate is held as `pending`. For tests and local development. */
export class InMemoryMemoryCandidateSink implements MemoryCandidateSink {
  private readonly held = new Map<string, HeldCandidate>()
  private nextReceipt = 1

  constructor(private readonly sourceVerifier?: MemoryCandidateSourceVerifier) {}

  async submit(agentId: string, candidate: MemoryCandidate): Promise<MemoryCandidateReceipt> {
    const payloadDigest = memoryCandidatePayloadDigest(candidate.record)
    const key = JSON.stringify([agentId, candidate.candidate_id])
    const existing = this.held.get(key)
    if (existing) {
      if (existing.payloadDigest === payloadDigest && existing.candidate.source === candidate.source) return structuredClone(existing.receipt)
      return {
        status: "rejected",
        candidate_id: candidate.candidate_id,
        receipt_id: this.mintReceiptId(),
        reason_code: "candidate_conflict",
      }
    }
    const verification = admitMemoryCandidate(agentId, candidate, this.sourceVerifier)
    if (!verification.valid) {
      return {
        status: "rejected",
        candidate_id: candidate.candidate_id,
        receipt_id: this.mintReceiptId(),
        reason_code: verification.reason_code,
      }
    }
    const receipt: MemoryCandidateReceipt = {
      status: "pending",
      candidate_id: candidate.candidate_id,
      receipt_id: this.mintReceiptId(),
    }
    this.held.set(key, { agentId, candidate: structuredClone(candidate), payloadDigest, receipt })
    return structuredClone(receipt)
  }

  /** Candidates held for `agentId`, in submission order. */
  candidates(agentId?: string): MemoryCandidate[] {
    return [...this.held.values()]
      .filter(held => agentId === undefined || held.agentId === agentId)
      .map(held => structuredClone(held.candidate))
  }

  private mintReceiptId(): string {
    return `receipt:${this.nextReceipt++}`
  }
}
