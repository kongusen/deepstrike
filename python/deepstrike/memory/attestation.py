"""Portable source attestation verification for memory candidates."""
from __future__ import annotations

import base64
import hashlib
import hmac
import json
import time
from dataclasses import dataclass
from typing import Literal, Mapping

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
except ImportError:  # pragma: no cover - exercised only in minimal installations
    Ed25519PublicKey = None

from .candidate_sink import MemoryCandidate, MemoryCandidateSource, memory_candidate_payload_digest


@dataclass(frozen=True)
class MemoryCandidateAttestation:
    issuer: str
    key_id: str
    agent_id: str
    tenant_id: str
    namespace: str
    candidate_id: str
    source: MemoryCandidateSource
    payload_digest: str
    issued_at: int
    expires_at: int
    policy_version: str
    signature: str

MemoryCandidateAttestationReasonCode = Literal[
    "attestation_missing", "attestation_verifier_unconfigured", "attestation_issuer_untrusted",
    "attestation_scope_mismatch", "attestation_candidate_mismatch", "attestation_source_mismatch",
    "attestation_digest_mismatch", "attestation_not_yet_valid", "attestation_expired",
    "attestation_signature_invalid", "host_verified_requires_host_author",
]


class Ed25519MemoryCandidateSourceVerifier:
    def __init__(self, trusted_keys: Mapping[str, tuple[str, str] | Mapping[str, str]], *, now=None,
                 allowed_clock_skew_ms: int = 60_000, max_lifetime_ms: int = 600_000,
                 policy_version: str = "memory-attestation-v1") -> None:
        self._trusted_keys = dict(trusted_keys)
        self._now = now or (lambda: int(time.time() * 1000))
        self._allowed_clock_skew_ms = allowed_clock_skew_ms
        self._max_lifetime_ms = max_lifetime_ms
        self._policy_version = policy_version

    def verify(self, agent_id: str, candidate: MemoryCandidate) -> tuple[bool, str | None]:
        attestation = candidate.attestation
        if attestation is None:
            return False, "attestation_missing"
        raw_key = self._trusted_keys.get(attestation.key_id)
        if raw_key is None:
            return False, "attestation_issuer_untrusted"
        issuer, public_key = (raw_key if isinstance(raw_key, tuple) else (raw_key["issuer"], raw_key["public_key"]))
        if issuer != attestation.issuer or attestation.agent_id != agent_id:
            return False, "attestation_scope_mismatch"
        if (attestation.tenant_id != candidate.record.scope.tenant_id
                or attestation.namespace != candidate.record.scope.namespace):
            return False, "attestation_scope_mismatch"
        if attestation.candidate_id != candidate.candidate_id:
            return False, "attestation_candidate_mismatch"
        if attestation.source != candidate.source:
            return False, "attestation_source_mismatch"
        if attestation.payload_digest != memory_candidate_payload_digest(candidate.record):
            return False, "attestation_digest_mismatch"
        if attestation.policy_version != self._policy_version:
            return False, "attestation_signature_invalid"
        if attestation.expires_at <= attestation.issued_at or attestation.expires_at - attestation.issued_at > self._max_lifetime_ms:
            return False, "attestation_expired"
        now = self._now()
        if now + self._allowed_clock_skew_ms < attestation.issued_at:
            return False, "attestation_not_yet_valid"
        if now - self._allowed_clock_skew_ms > attestation.expires_at:
            return False, "attestation_expired"
        if not _verify_ed25519(public_key, memory_candidate_attestation_canonical_payload(attestation).encode(), attestation.signature):
            return False, "attestation_signature_invalid"
        return True, None


class HmacMemoryCandidateSourceVerifier:
    def __init__(self, trusted_keys: Mapping[str, tuple[str, str] | Mapping[str, str]], *, now=None,
                 allowed_clock_skew_ms: int = 60_000, max_lifetime_ms: int = 600_000,
                 policy_version: str = "memory-attestation-v1") -> None:
        self._trusted_keys = dict(trusted_keys)
        self._now = now or (lambda: int(time.time() * 1000))
        self._allowed_clock_skew_ms = allowed_clock_skew_ms
        self._max_lifetime_ms = max_lifetime_ms
        self._policy_version = policy_version

    def verify(self, agent_id: str, candidate: MemoryCandidate) -> tuple[bool, str | None]:
        attestation = candidate.attestation
        if attestation is None:
            return False, "attestation_missing"
        raw_key = self._trusted_keys.get(attestation.key_id)
        if raw_key is None:
            return False, "attestation_issuer_untrusted"
        issuer, key = (raw_key if isinstance(raw_key, tuple) else (raw_key["issuer"], raw_key["secret"]))
        if issuer != attestation.issuer or attestation.agent_id != agent_id:
            return False, "attestation_scope_mismatch"
        if (attestation.tenant_id != candidate.record.scope.tenant_id
                or attestation.namespace != candidate.record.scope.namespace):
            return False, "attestation_scope_mismatch"
        if attestation.candidate_id != candidate.candidate_id:
            return False, "attestation_candidate_mismatch"
        if attestation.source != candidate.source:
            return False, "attestation_source_mismatch"
        if attestation.payload_digest != memory_candidate_payload_digest(candidate.record):
            return False, "attestation_digest_mismatch"
        if attestation.policy_version != self._policy_version or attestation.expires_at <= attestation.issued_at:
            return False, "attestation_signature_invalid"
        if attestation.expires_at - attestation.issued_at > self._max_lifetime_ms:
            return False, "attestation_expired"
        now = self._now()
        if now + self._allowed_clock_skew_ms < attestation.issued_at:
            return False, "attestation_not_yet_valid"
        if now - self._allowed_clock_skew_ms > attestation.expires_at:
            return False, "attestation_expired"
        expected = sign_memory_candidate_attestation(attestation, key)
        if not hmac.compare_digest(expected, attestation.signature):
            return False, "attestation_signature_invalid"
        return True, None


def admit_memory_candidate(agent_id: str, candidate: MemoryCandidate, verifier: object | None = None) -> tuple[bool, str | None]:
    return verify_memory_candidate_source(agent_id, candidate, verifier)


def verify_memory_candidate_source(agent_id: str, candidate: MemoryCandidate, verifier: object | None = None) -> tuple[bool, str | None]:
    if candidate.record.provenance.trust == "host_verified" and candidate.record.provenance.author != "host":
        return False, "host_verified_requires_host_author"
    requires = candidate.record.provenance.author == "host" and candidate.record.provenance.trust == "host_verified"
    if candidate.attestation is None:
        return (False, "attestation_missing") if requires else (True, None)
    if verifier is None:
        return False, "attestation_verifier_unconfigured"
    return verifier.verify(agent_id, candidate)


def memory_candidate_attestation_canonical_payload(attestation: MemoryCandidateAttestation) -> str:
    return json.dumps([
        attestation.issuer, attestation.key_id, attestation.agent_id, attestation.tenant_id,
        attestation.namespace, attestation.candidate_id, attestation.source, attestation.payload_digest,
        attestation.issued_at, attestation.expires_at, attestation.policy_version,
    ], ensure_ascii=False, separators=(",", ":"))


def memory_candidate_attestation_digest(attestation: MemoryCandidateAttestation | None) -> str | None:
    if attestation is None:
        return None
    return "sha256:" + hashlib.sha256(memory_candidate_attestation_canonical_payload(attestation).encode()).hexdigest()


def sign_memory_candidate_attestation(attestation: MemoryCandidateAttestation, key: str) -> str:
    return "hmac-sha256:" + hmac.new(key.encode(), memory_candidate_attestation_canonical_payload(attestation).encode(), hashlib.sha256).hexdigest()


def _verify_ed25519(public_key: str, message: bytes, signature: str) -> bool:
    if not signature.startswith("ed25519:"):
        return False
    try:
        key = _decode_base64url(public_key)
        sig = _decode_base64url(signature[9:])
        if len(key) != 32 or len(sig) != 64:
            return False
        if Ed25519PublicKey is None:
            return False
        Ed25519PublicKey.from_public_bytes(key).verify(sig, message)
        return True
    except (ValueError, TypeError, UnicodeError):
        return False
    except Exception:
        return False


def _decode_base64url(value: str) -> bytes:
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
