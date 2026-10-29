import { createHash, createHmac, createPublicKey, timingSafeEqual, verify as verifySignature, type KeyObject } from "node:crypto"

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

/** Verifies host-issued candidate source attestations with Ed25519 public keys. */
export class Ed25519MemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  private readonly trustedKeys: Readonly<Record<string, { issuer: string; publicKey: string }>>
  private readonly now: () => number
  private readonly allowedClockSkewMs: number
  private readonly maxLifetimeMs: number
  private readonly policyVersion: string

  constructor(options: Ed25519MemoryCandidateSourceVerifierOptions) {
    this.trustedKeys = options.trustedKeys
    this.now = options.now ?? (() => Date.now())
    this.allowedClockSkewMs = options.allowedClockSkewMs ?? 60_000
    this.maxLifetimeMs = options.maxLifetimeMs ?? 600_000
    this.policyVersion = options.policyVersion ?? "memory-attestation-v1"
  }

  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification {
    const attestation = candidate.attestation
    if (!attestation) return { valid: false, reason_code: "attestation_missing" }
    const key = this.trustedKeys[attestation.key_id]
    if (!key || key.issuer !== attestation.issuer) return { valid: false, reason_code: "attestation_issuer_untrusted" }
    if (attestation.agent_id !== agentId) return { valid: false, reason_code: "attestation_scope_mismatch" }
    if (attestation.tenant_id !== candidate.record.scope.tenant_id || attestation.namespace !== candidate.record.scope.namespace) {
      return { valid: false, reason_code: "attestation_scope_mismatch" }
    }
    if (attestation.candidate_id !== candidate.candidate_id) return { valid: false, reason_code: "attestation_candidate_mismatch" }
    if (attestation.source !== candidate.source) return { valid: false, reason_code: "attestation_source_mismatch" }
    if (attestation.payload_digest !== memoryCandidatePayloadDigest(candidate.record)) {
      return { valid: false, reason_code: "attestation_digest_mismatch" }
    }
    if (attestation.policy_version !== this.policyVersion) return { valid: false, reason_code: "attestation_signature_invalid" }
    if (!Number.isSafeInteger(attestation.issued_at) || !Number.isSafeInteger(attestation.expires_at)
      || attestation.expires_at <= attestation.issued_at
      || attestation.expires_at - attestation.issued_at > this.maxLifetimeMs) {
      return { valid: false, reason_code: "attestation_expired" }
    }
    const now = this.now()
    if (now + this.allowedClockSkewMs < attestation.issued_at) return { valid: false, reason_code: "attestation_not_yet_valid" }
    if (now - this.allowedClockSkewMs > attestation.expires_at) return { valid: false, reason_code: "attestation_expired" }
    const signature = decodeBase64UrlSignature(attestation.signature)
    if (!signature || signature.length !== 64) return { valid: false, reason_code: "attestation_signature_invalid" }
    try {
      const publicKey = ed25519PublicKey(key.publicKey)
      if (!verifySignature(null, Buffer.from(memoryCandidateAttestationCanonicalPayload(attestation)), publicKey, signature)) {
        return { valid: false, reason_code: "attestation_signature_invalid" }
      }
    } catch {
      return { valid: false, reason_code: "attestation_signature_invalid" }
    }
    return { valid: true, attestation, verified_at: now }
  }
}

/** Verifies host-issued candidate source attestations with a small, portable HMAC contract. */
export class HmacMemoryCandidateSourceVerifier implements MemoryCandidateSourceVerifier {
  private readonly trustedKeys: Readonly<Record<string, { issuer: string; secret: string }>>
  private readonly now: () => number
  private readonly allowedClockSkewMs: number
  private readonly maxLifetimeMs: number
  private readonly policyVersion: string

  constructor(options: HmacMemoryCandidateSourceVerifierOptions) {
    this.trustedKeys = options.trustedKeys
    this.now = options.now ?? (() => Date.now())
    this.allowedClockSkewMs = options.allowedClockSkewMs ?? 60_000
    this.maxLifetimeMs = options.maxLifetimeMs ?? 600_000
    this.policyVersion = options.policyVersion ?? "memory-attestation-v1"
  }

  verify(agentId: string, candidate: MemoryCandidate): MemoryCandidateVerification {
    const attestation = candidate.attestation
    if (!attestation) return { valid: false, reason_code: "attestation_missing" }
    const key = this.trustedKeys[attestation.key_id]
    if (!key || key.issuer !== attestation.issuer) return { valid: false, reason_code: "attestation_issuer_untrusted" }
    if (attestation.agent_id !== agentId) return { valid: false, reason_code: "attestation_scope_mismatch" }
    if (attestation.tenant_id !== candidate.record.scope.tenant_id || attestation.namespace !== candidate.record.scope.namespace) {
      return { valid: false, reason_code: "attestation_scope_mismatch" }
    }
    if (attestation.candidate_id !== candidate.candidate_id) return { valid: false, reason_code: "attestation_candidate_mismatch" }
    if (attestation.source !== candidate.source) return { valid: false, reason_code: "attestation_source_mismatch" }
    if (attestation.payload_digest !== memoryCandidatePayloadDigest(candidate.record)) {
      return { valid: false, reason_code: "attestation_digest_mismatch" }
    }
    if (attestation.policy_version !== this.policyVersion) return { valid: false, reason_code: "attestation_signature_invalid" }
    if (!Number.isSafeInteger(attestation.issued_at) || !Number.isSafeInteger(attestation.expires_at)
      || attestation.expires_at <= attestation.issued_at
      || attestation.expires_at - attestation.issued_at > this.maxLifetimeMs) {
      return { valid: false, reason_code: "attestation_expired" }
    }
    const now = this.now()
    if (now + this.allowedClockSkewMs < attestation.issued_at) return { valid: false, reason_code: "attestation_not_yet_valid" }
    if (now - this.allowedClockSkewMs > attestation.expires_at) return { valid: false, reason_code: "attestation_expired" }
    const expected = signMemoryCandidateAttestation(attestation, key.secret)
    if (!constantTimeEqual(expected, attestation.signature)) return { valid: false, reason_code: "attestation_signature_invalid" }
    return { valid: true, attestation, verified_at: now }
  }
}

export function verifyMemoryCandidateSource(
  agentId: string,
  candidate: MemoryCandidate,
  verifier?: MemoryCandidateSourceVerifier,
): MemoryCandidateVerification {
  if (candidate.record.provenance.trust === "host_verified" && candidate.record.provenance.author !== "host") {
    return { valid: false, reason_code: "host_verified_requires_host_author" }
  }
  const requiresAttestation = candidate.record.provenance.author === "host" && candidate.record.provenance.trust === "host_verified"
  if (!candidate.attestation) {
    return requiresAttestation
      ? { valid: false, reason_code: "attestation_missing" }
      : { valid: true }
  }
  if (!verifier) return { valid: false, reason_code: "attestation_verifier_unconfigured" }
  return verifier.verify(agentId, candidate)
}

export function memoryCandidateAttestationDigest(attestation: MemoryCandidateAttestation | undefined): string | undefined {
  if (!attestation) return undefined
  return `sha256:${createHash("sha256").update(memoryCandidateAttestationCanonicalPayload(attestation)).digest("hex")}`
}

export function memoryCandidateAttestationCanonicalPayload(attestation: Omit<MemoryCandidateAttestation, "signature">): string {
  return JSON.stringify([
    attestation.issuer, attestation.key_id, attestation.agent_id, attestation.tenant_id,
    attestation.namespace, attestation.candidate_id, attestation.source, attestation.payload_digest,
    attestation.issued_at, attestation.expires_at, attestation.policy_version,
  ])
}

export function signMemoryCandidateAttestation(attestation: Omit<MemoryCandidateAttestation, "signature">, key: string): string {
  return `hmac-sha256:${createHmac("sha256", key).update(memoryCandidateAttestationCanonicalPayload(attestation)).digest("hex")}`
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

function constantTimeEqual(left: string, right: string): boolean {
  const leftBytes = Buffer.from(left)
  const rightBytes = Buffer.from(right)
  return leftBytes.length === rightBytes.length && timingSafeEqual(leftBytes, rightBytes)
}
