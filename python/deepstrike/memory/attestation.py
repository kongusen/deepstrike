"""Portable source attestation verification for memory candidates."""
from __future__ import annotations

import base64
import time
from dataclasses import asdict, dataclass
from typing import Literal, Mapping

try:
    from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
except ImportError:  # pragma: no cover - exercised only in minimal installations
    Ed25519PublicKey = None

from .authority import ask_memory_authority
from .candidate_sink import MemoryCandidate, MemoryCandidateSource


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


def _candidate_wire(candidate: MemoryCandidate) -> dict:
    wire = {"candidate_id": candidate.candidate_id, "source": candidate.source, "record": asdict(candidate.record)}
    if candidate.attestation is not None:
        wire["attestation"] = asdict(candidate.attestation)
    return wire


def _wire_timestamps(attestation: MemoryCandidateAttestation) -> bool:
    return all(isinstance(value, int) and not isinstance(value, bool) and value >= 0
               for value in (attestation.issued_at, attestation.expires_at))


class _KernelCheckedVerifier:
    """Key lookup and settings; the kernel makes every check but an Ed25519 signature."""

    _secret_field = ""

    def __init__(self, trusted_keys: Mapping[str, tuple[str, str] | Mapping[str, str]], *, now=None,
                 allowed_clock_skew_ms: int = 60_000, max_lifetime_ms: int = 600_000,
                 policy_version: str = "memory-attestation-v1") -> None:
        self._trusted_keys = dict(trusted_keys)
        self._now = now or (lambda: int(time.time() * 1000))
        self._allowed_clock_skew_ms = allowed_clock_skew_ms
        self._max_lifetime_ms = max_lifetime_ms
        self._policy_version = policy_version

    def _key(self, candidate: MemoryCandidate) -> tuple[str, str] | None:
        raw_key = self._trusted_keys.get(candidate.attestation.key_id) if candidate.attestation else None
        if raw_key is None:
            return None
        return raw_key if isinstance(raw_key, tuple) else (raw_key["issuer"], raw_key[self._secret_field])

    def _check(self, agent_id: str, candidate: MemoryCandidate, *, hmac_secret: str | None = None) -> dict:
        attestation = candidate.attestation
        if attestation is None:
            return {"valid": False, "reason_code": "attestation_missing"}
        key = self._key(candidate)
        if key is None:
            return {"valid": False, "reason_code": "attestation_issuer_untrusted"}
        if not _wire_timestamps(attestation):
            return {"valid": False, "reason_code": "attestation_expired"}
        return ask_memory_authority({
            "op": "verify_source",
            "agent_id": agent_id,
            "candidate": _candidate_wire(candidate),
            "verifier": {
                "trusted_issuer": key[0],
                **({"hmac_secret": hmac_secret} if hmac_secret is not None else {}),
                "window": {
                    "now_ms": max(0, int(self._now())),
                    "allowed_clock_skew_ms": self._allowed_clock_skew_ms,
                    "max_lifetime_ms": self._max_lifetime_ms,
                    "policy_version": self._policy_version,
                },
            },
        })


class Ed25519MemoryCandidateSourceVerifier(_KernelCheckedVerifier):
    _secret_field = "public_key"

    def verify(self, agent_id: str, candidate: MemoryCandidate) -> tuple[bool, str | None]:
        answer = self._check(agent_id, candidate)
        if not answer.get("valid"):
            return False, answer.get("reason_code")
        signed_payload = answer.get("signed_payload")
        if not isinstance(signed_payload, str) or not _verify_ed25519(self._key(candidate)[1], signed_payload.encode(), candidate.attestation.signature):
            return False, "attestation_signature_invalid"
        return True, None


class HmacMemoryCandidateSourceVerifier(_KernelCheckedVerifier):
    _secret_field = "secret"

    def verify(self, agent_id: str, candidate: MemoryCandidate) -> tuple[bool, str | None]:
        key = self._key(candidate)
        answer = self._check(agent_id, candidate, hmac_secret=key[1] if key else None)
        if not answer.get("valid"):
            return False, answer.get("reason_code")
        return True, None


def admit_memory_candidate(agent_id: str, candidate: MemoryCandidate, verifier: object | None = None) -> tuple[bool, str | None]:
    return verify_memory_candidate_source(agent_id, candidate, verifier)


def verify_memory_candidate_source(agent_id: str, candidate: MemoryCandidate, verifier: object | None = None) -> tuple[bool, str | None]:
    """The kernel's provenance rule, then the host's verifier for any attestation supplied."""
    # Asked with no verifier, the kernel admits a candidate that needs none, refuses a bad claim,
    # and answers ``attestation_verifier_unconfigured`` for one whose attestation the host judges.
    if candidate.attestation is not None and not _wire_timestamps(candidate.attestation):
        return verifier.verify(agent_id, candidate) if verifier is not None else (False, "attestation_verifier_unconfigured")
    answer = ask_memory_authority({
        "op": "verify_source", "agent_id": agent_id, "candidate": _candidate_wire(candidate),
    })
    if answer.get("valid"):
        return True, None
    if answer.get("reason_code") == "attestation_verifier_unconfigured" and verifier is not None:
        return verifier.verify(agent_id, candidate)
    return False, answer.get("reason_code")


def memory_candidate_attestation_canonical_payload(attestation: MemoryCandidateAttestation) -> str:
    return str(ask_memory_authority({"op": "attestation", "attestation": asdict(attestation)})["canonical_payload"])


def memory_candidate_attestation_digest(attestation: MemoryCandidateAttestation | None) -> str | None:
    if attestation is None or not _wire_timestamps(attestation):
        return None
    return str(ask_memory_authority({"op": "attestation", "attestation": asdict(attestation)})["digest"])


def sign_memory_candidate_attestation(attestation: MemoryCandidateAttestation, key: str) -> str:
    return str(ask_memory_authority({"op": "attestation", "attestation": asdict(attestation), "secret": key})["signature"])


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
