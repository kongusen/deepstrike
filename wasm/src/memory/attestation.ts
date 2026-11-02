import { askMemoryAuthority } from "./authority.js"
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
  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification | Promise<MemoryCandidateVerification>
}

/** Browser-safe verifier seam. Signature verification stays host-injected in WASM. */
export class HostMemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  constructor(private readonly verifyFn: (agentId: string, candidate: MemoryCandidate) => MemoryCandidateVerification | Promise<MemoryCandidateVerification>) {}
  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification | Promise<MemoryCandidateVerification> { return this.verifyFn(agentId, candidate) }
}

/** Timestamps the kernel can read: non-negative safe integers. */
function wireTimestamps(attestation: Pick<MemoryCandidateAttestation, "issued_at" | "expires_at">): boolean {
  return [attestation.issued_at, attestation.expires_at].every(value => Number.isSafeInteger(value) && value >= 0)
}

/** The kernel's provenance rule, then the host's verifier: only the host may claim
 *  `host_verified`, that claim must carry an attestation, and any attestation supplied is verified. */
export async function verifyMemoryCandidateSource(agentId: string, candidate: MemoryCandidate, verifier?: MemoryCandidateSourceVerifier): Promise<MemoryCandidateVerification> {
  // Asked with no verifier, the kernel admits a candidate that needs none, refuses a bad claim, and
  // answers `attestation_verifier_unconfigured` for one whose attestation the host must judge.
  if (candidate.attestation && !wireTimestamps(candidate.attestation)) {
    return verifier ? verifier.verify(agentId, candidate) : { valid: false, reason_code: "attestation_verifier_unconfigured" }
  }
  const answer = await askMemoryAuthority({ op: "verify_source", agent_id: agentId, candidate })
  if (answer.valid === true) return { valid: true }
  if (answer.reason_code === "attestation_verifier_unconfigured" && verifier) return verifier.verify(agentId, candidate)
  return { valid: false, reason_code: String(answer.reason_code) }
}

export async function admitMemoryCandidate(agentId: string, candidate: MemoryCandidate, verifier?: MemoryCandidateSourceVerifier): Promise<MemoryCandidateVerification> {
  return verifyMemoryCandidateSource(agentId, candidate, verifier)
}

export async function memoryCandidateAttestationCanonicalPayload(attestation: Omit<MemoryCandidateAttestation, "signature">): Promise<string> {
  return String((await askMemoryAuthority({ op: "attestation", attestation })).canonical_payload)
}

export async function memoryCandidateAttestationDigest(attestation: MemoryCandidateAttestation): Promise<string> {
  return String((await askMemoryAuthority({ op: "attestation", attestation })).digest)
}

export async function memoryCandidateAttestationMatches(candidate: MemoryCandidate, attestation: MemoryCandidateAttestation): Promise<boolean> {
  return attestation.candidate_id === candidate.candidate_id &&
    attestation.source === candidate.source &&
    attestation.payload_digest === await memoryCandidatePayloadDigest(candidate.record)
}
