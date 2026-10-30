import { sha256Hex } from "../runtime/sha256.js"
import { memoryCandidatePayloadDigest, type MemoryCandidate, type MemoryCandidateSource } from "./candidate-sink.js"

export interface MemoryCandidateAttestation {
  issuer: string
  key_id: string
  agent_id: string
  tenant_id: string
  namespace: string
  candidate_id: string
  source: MemoryCandidateSource
  payload_digest: string
  issued_at: number
  expires_at: number
  policy_version: string
  signature: string
}

export type MemoryCandidateVerification =
  | { valid: true; verified_at?: number }
  | { valid: false; reason_code: string }

export interface MemoryCandidateSourceVerifier {
  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification
}

/** Browser-safe verifier seam. Signature verification stays host-injected in WASM. */
export class HostMemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  constructor(private readonly verifyFn: (agentId: string, candidate: MemoryCandidate) => MemoryCandidateVerification) {}
  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification { return this.verifyFn(agentId, candidate) }
}

export function verifyMemoryCandidateSource(agentId: string, candidate: MemoryCandidate, verifier?: MemoryCandidateSourceVerifier): MemoryCandidateVerification {
  if (candidate.record.provenance.trust === "host_verified" && candidate.record.provenance.author !== "host") return { valid: false, reason_code: "host_verified_requires_host_author" }
  const requires = candidate.record.provenance.author === "host" && candidate.record.provenance.trust === "host_verified"
  if (!candidate.attestation) return requires ? { valid: false, reason_code: "attestation_missing" } : { valid: true }
  if (!verifier) return { valid: false, reason_code: "attestation_verifier_unconfigured" }
  return verifier.verify(agentId, candidate)
}

export function admitMemoryCandidate(agentId: string, candidate: MemoryCandidate, verifier?: MemoryCandidateSourceVerifier): MemoryCandidateVerification {
  return verifyMemoryCandidateSource(agentId, candidate, verifier)
}

export function memoryCandidateAttestationCanonicalPayload(attestation: Omit<MemoryCandidateAttestation, "signature">): string {
  return JSON.stringify([
    attestation.issuer, attestation.key_id, attestation.agent_id, attestation.tenant_id,
    attestation.namespace, attestation.candidate_id, attestation.source, attestation.payload_digest,
    attestation.issued_at, attestation.expires_at, attestation.policy_version,
  ])
}

export function memoryCandidateAttestationDigest(attestation: MemoryCandidateAttestation): string {
  return sha256Hex(memoryCandidateAttestationCanonicalPayload(attestation))
}

export function memoryCandidateAttestationMatches(candidate: MemoryCandidate, attestation: MemoryCandidateAttestation): boolean {
  return attestation.candidate_id === candidate.candidate_id &&
    attestation.source === candidate.source &&
    attestation.payload_digest === memoryCandidatePayloadDigest(candidate.record)
}
