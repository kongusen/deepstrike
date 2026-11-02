import { createPublicKey, verify as verifySignature, type KeyObject } from "node:crypto"

import { askMemoryAuthority } from "./authority.js"
import type { MemoryCandidate, MemoryCandidateSource } from "./candidate-sink.js"

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

export type MemoryCandidateAttestationReasonCode =
  | "attestation_missing"
  | "attestation_verifier_unconfigured"
  | "attestation_issuer_untrusted"
  | "attestation_scope_mismatch"
  | "attestation_candidate_mismatch"
  | "attestation_source_mismatch"
  | "attestation_digest_mismatch"
  | "attestation_not_yet_valid"
  | "attestation_expired"
  | "attestation_signature_invalid"
  | "host_verified_requires_host_author"

export type MemoryCandidateVerification =
  | { valid: true; attestation?: MemoryCandidateAttestation; verified_at?: number }
  | { valid: false; reason_code: MemoryCandidateAttestationReasonCode }

export interface MemoryCandidateSourceVerifier {
  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification
}

export interface HmacMemoryCandidateSourceVerifierOptions {
  trustedKeys: Readonly<Record<string, { issuer: string; secret: string }>>
  now?: () => number
  allowedClockSkewMs?: number
  maxLifetimeMs?: number
  policyVersion?: string
}

export interface Ed25519MemoryCandidateSourceVerifierOptions {
  trustedKeys: Readonly<Record<string, { issuer: string; publicKey: string }>>
  now?: () => number
  allowedClockSkewMs?: number
  maxLifetimeMs?: number
  policyVersion?: string
}

interface VerifierSettings {
  now: () => number
  allowedClockSkewMs: number
  maxLifetimeMs: number
  policyVersion: string
}

function verifierSettings(options: { now?: () => number; allowedClockSkewMs?: number; maxLifetimeMs?: number; policyVersion?: string }): VerifierSettings {
  return {
    now: options.now ?? (() => Date.now()),
    allowedClockSkewMs: options.allowedClockSkewMs ?? 60_000,
    maxLifetimeMs: options.maxLifetimeMs ?? 600_000,
    policyVersion: options.policyVersion ?? "memory-attestation-v1",
  }
}

/** Timestamps the kernel can read: non-negative safe integers. Anything else is an expired claim. */
function wireTimestamps(attestation: Pick<MemoryCandidateAttestation, "issued_at" | "expires_at">): boolean {
  return [attestation.issued_at, attestation.expires_at].every(value => Number.isSafeInteger(value) && value >= 0)
}

/** The kernel's checks of an attestation, short of an Ed25519 signature. With `hmacSecret` the
 *  kernel checks the signature too; without it a passing answer carries the bytes the host still
 *  has to verify. */
function checkAttestation(
  agentId: string,
  candidate: MemoryCandidate,
  key: { issuer: string } | undefined,
  settings: VerifierSettings,
  hmacSecret?: string,
): { valid: true; now: number; signedPayload?: string } | { valid: false; reason_code: MemoryCandidateAttestationReasonCode } {
  const attestation = candidate.attestation
  if (!attestation) return { valid: false, reason_code: "attestation_missing" }
  if (!key) return { valid: false, reason_code: "attestation_issuer_untrusted" }
  if (!wireTimestamps(attestation)) return { valid: false, reason_code: "attestation_expired" }
  const now = settings.now()
  const answer = askMemoryAuthority({
    op: "verify_source",
    agent_id: agentId,
    candidate,
    verifier: {
      trusted_issuer: key.issuer,
      ...(hmacSecret !== undefined ? { hmac_secret: hmacSecret } : {}),
      window: {
        now_ms: Math.max(0, Math.trunc(now)),
        allowed_clock_skew_ms: settings.allowedClockSkewMs,
        max_lifetime_ms: settings.maxLifetimeMs,
        policy_version: settings.policyVersion,
      },
    },
  })
  if (answer.valid !== true) return { valid: false, reason_code: answer.reason_code as MemoryCandidateAttestationReasonCode }
  return { valid: true, now, ...(typeof answer.signed_payload === "string" ? { signedPayload: answer.signed_payload } : {}) }
}

/** Verifies host-issued candidate source attestations with Ed25519 public keys. Every check but
 *  the signature is the kernel's; the signature is verified here, so the key never leaves Node. */
export class Ed25519MemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  private readonly trustedKeys: Readonly<Record<string, { issuer: string; publicKey: string }>>
  private readonly settings: VerifierSettings

  constructor(options: Ed25519MemoryCandidateSourceVerifierOptions) {
    this.trustedKeys = options.trustedKeys
    this.settings = verifierSettings(options)
  }

  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification {
    const key = candidate.attestation ? this.trustedKeys[candidate.attestation.key_id] : undefined
    const checked = checkAttestation(agentId, candidate, key, this.settings)
    if (!checked.valid) return checked
    const attestation = candidate.attestation!
    const signature = decodeBase64UrlSignature(attestation.signature)
    if (!signature || signature.length !== 64 || checked.signedPayload === undefined) {
      return { valid: false, reason_code: "attestation_signature_invalid" }
    }
    try {
      if (!verifySignature(null, Buffer.from(checked.signedPayload), ed25519PublicKey(key!.publicKey), signature)) {
        return { valid: false, reason_code: "attestation_signature_invalid" }
      }
    } catch {
      return { valid: false, reason_code: "attestation_signature_invalid" }
    }
    return { valid: true, attestation, verified_at: checked.now }
  }
}

/** Verifies host-issued candidate source attestations with a small, portable HMAC contract. The
 *  kernel makes every check, the signature included. */
export class HmacMemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  private readonly trustedKeys: Readonly<Record<string, { issuer: string; secret: string }>>
  private readonly settings: VerifierSettings

  constructor(options: HmacMemoryCandidateSourceVerifierOptions) {
    this.trustedKeys = options.trustedKeys
    this.settings = verifierSettings(options)
  }

  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification {
    const key = candidate.attestation ? this.trustedKeys[candidate.attestation.key_id] : undefined
    const checked = checkAttestation(agentId, candidate, key, this.settings, key?.secret)
    if (!checked.valid) return checked
    return { valid: true, attestation: candidate.attestation, verified_at: checked.now }
  }
}

/** The kernel's provenance rule, then the host's verifier: only the host may claim
 *  `host_verified`, that claim must carry an attestation, and any attestation supplied is verified. */
export function verifyMemoryCandidateSource(
  agentId: string,
  candidate: MemoryCandidate,
  verifier?: MemoryCandidateSourceVerifier,
): MemoryCandidateVerification {
  // Asked with no verifier, the kernel admits a candidate that needs none, refuses a bad claim, and
  // answers `attestation_verifier_unconfigured` for one whose attestation the host must judge.
  if (candidate.attestation && !wireTimestamps(candidate.attestation)) {
    return verifier ? verifier.verify(agentId, candidate) : { valid: false, reason_code: "attestation_verifier_unconfigured" }
  }
  const answer = askMemoryAuthority({ op: "verify_source", agent_id: agentId, candidate })
  if (answer.valid === true) return { valid: true }
  if (answer.reason_code === "attestation_verifier_unconfigured" && verifier) return verifier.verify(agentId, candidate)
  return { valid: false, reason_code: answer.reason_code as MemoryCandidateAttestationReasonCode }
}

export function memoryCandidateAttestationDigest(attestation: MemoryCandidateAttestation | undefined): string | undefined {
  if (!attestation || !wireTimestamps(attestation)) return undefined
  return String(askMemoryAuthority({ op: "attestation", attestation }).digest)
}

export function memoryCandidateAttestationCanonicalPayload(attestation: Omit<MemoryCandidateAttestation, "signature">): string {
  return String(askMemoryAuthority({ op: "attestation", attestation }).canonical_payload)
}

export function signMemoryCandidateAttestation(attestation: Omit<MemoryCandidateAttestation, "signature">, key: string): string {
  return String(askMemoryAuthority({ op: "attestation", attestation, secret: key }).signature)
}

function ed25519PublicKey(publicKey: string): KeyObject {
  const raw = decodeBase64Url(publicKey)
  if (!raw || raw.length !== 32) throw new Error("Ed25519 public key must be 32 bytes")
  return createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, raw]), format: "der", type: "spki" })
}

function decodeBase64UrlSignature(signature: string): Buffer | undefined {
  if (!signature.startsWith("ed25519:")) return undefined
  return decodeBase64Url(signature.slice("ed25519:".length))
}

function decodeBase64Url(value: string): Buffer | undefined {
  if (!/^[A-Za-z0-9_-]+$/.test(value) || value.length % 4 === 1) return undefined
  try { return Buffer.from(value.replace(/-/g, "+").replace(/_/g, "/"), "base64") } catch { return undefined }
}

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex")

export function admitMemoryCandidate(agentId: string, candidate: MemoryCandidate, verifier?: MemoryCandidateSourceVerifier): MemoryCandidateVerification {
  return verifyMemoryCandidateSource(agentId, candidate, verifier)
}
