"""Candidate hand-off: pending is terminal, but is not a remembered record."""
from __future__ import annotations

import hashlib
import json
from copy import deepcopy
from dataclasses import dataclass
from typing import Any, Literal, Protocol, Sequence, TypedDict, runtime_checkable

from .protocols import MemoryRecord

MemoryCandidateSource = Literal["session_extraction", "semantic_archive", "child_request"]


@dataclass(frozen=True)
class MemoryCandidate:
    candidate_id: str
    source: MemoryCandidateSource
    record: MemoryRecord


class PendingMemoryCandidateReceipt(TypedDict):
    status: Literal["pending"]
    candidate_id: str
    receipt_id: str


class AcceptedMemoryCandidateReceipt(TypedDict):
    status: Literal["accepted"]
    candidate_id: str
    receipt_id: str
    record_ref: str
    digest: str


class RejectedMemoryCandidateReceipt(TypedDict):
    status: Literal["rejected"]
    candidate_id: str
    receipt_id: str
    reason_code: str


MemoryCandidateReceipt = PendingMemoryCandidateReceipt | AcceptedMemoryCandidateReceipt | RejectedMemoryCandidateReceipt


@runtime_checkable
class MemoryCandidateSink(Protocol):
    """Same id/payload returns the original receipt; conflicting payload must be rejected.

    Raising means no answer was obtained, and permits redelivery of the same candidate.
    An accepted receipt must identify the record actually held by the host.
    """

    async def submit(self, agent_id: str, candidate: MemoryCandidate) -> MemoryCandidateReceipt: ...


def _json_digest(parts: Sequence[Any]) -> str:
    # Match JSON.stringify's compact array identity, including non-ASCII text.
    normalized = [int(value) if isinstance(value, float) and value.is_integer() else value for value in parts]
    return hashlib.sha256(json.dumps(normalized, ensure_ascii=False, separators=(",", ":")).encode("utf-8")).hexdigest()


def memory_candidate_id(parts: Sequence[str]) -> str:
    return "candidate:" + _json_digest(parts)


def memory_candidate_payload_digest(record: MemoryRecord) -> str:
    """Compare review metadata, excluding store IDs, timestamps and recall counters."""
    return "sha256:" + _json_digest([
        record.scope.tenant_id, record.scope.namespace, record.kind,
        record.name, record.description, record.content,
        record.provenance.author, record.provenance.trust, record.provenance.session_id,
        record.provenance.evidence_refs, record.confidence, record.links, record.pinned, record.ttl_days,
    ])


class InMemoryMemoryCandidateSink:
    """In-process pending candidate store for tests and local development."""

    def __init__(self) -> None:
        self._held: dict[tuple[str, str], tuple[str, MemoryCandidate, str, MemoryCandidateReceipt]] = {}
        self._next_receipt = 1

    def _mint_receipt_id(self) -> str:
        receipt_id = f"receipt:{self._next_receipt}"
        self._next_receipt += 1
        return receipt_id

    async def submit(self, agent_id: str, candidate: MemoryCandidate) -> MemoryCandidateReceipt:
        digest = memory_candidate_payload_digest(candidate.record)
        key = (agent_id, candidate.candidate_id)
        existing = self._held.get(key)
        if existing is not None:
            if existing[2] == digest and existing[1].source == candidate.source:
                return deepcopy(existing[3])
            return {
                "status": "rejected", "candidate_id": candidate.candidate_id,
                "receipt_id": self._mint_receipt_id(), "reason_code": "candidate_conflict",
            }
        receipt: MemoryCandidateReceipt = {
            "status": "pending", "candidate_id": candidate.candidate_id,
            "receipt_id": self._mint_receipt_id(),
        }
        self._held[key] = (agent_id, deepcopy(candidate), digest, receipt)
        return deepcopy(receipt)

    def candidates(self, agent_id: str | None = None) -> list[MemoryCandidate]:
        """Held candidates in submission order, optionally filtered by agent."""
        return [deepcopy(candidate) for owner, candidate, _, _ in self._held.values()
                if agent_id is None or owner == agent_id]
