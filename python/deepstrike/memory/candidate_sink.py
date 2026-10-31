"""Candidate hand-off: pending is terminal, but is not a remembered record."""
from __future__ import annotations

from copy import deepcopy
from dataclasses import asdict, dataclass
from typing import TYPE_CHECKING, Any, Literal, Protocol, Sequence, TypedDict, runtime_checkable

from .authority import ask_memory_authority
from .protocols import MemoryRecord

if TYPE_CHECKING:
    from .attestation import MemoryCandidateAttestation

MemoryCandidateSource = Literal["child_request", "model_proposal"]


@dataclass(frozen=True)
class MemoryCandidate:
    candidate_id: str
    source: MemoryCandidateSource
    record: MemoryRecord
    attestation: "MemoryCandidateAttestation | None" = None


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


def memory_candidate_id(parts: Sequence[str]) -> str:
    """A candidate id from the write's canonical identity, minted by the kernel."""
    return str(ask_memory_authority({"op": "candidate_id", "parts": [str(part) for part in parts]})["candidate_id"])


def memory_candidate_payload_digest(record: MemoryRecord) -> str:
    """The kernel's digest of what a candidate proposes, excluding store IDs, timestamps and recall counters."""
    return str(ask_memory_authority({"op": "candidate_payload_digest", "record": asdict(record)})["payload_digest"])


class InMemoryMemoryCandidateSink:
    """In-process candidate store with the shared source-admission boundary."""

    def __init__(self, source_verifier=None) -> None:
        self._source_verifier = source_verifier
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
        from .attestation import admit_memory_candidate
        valid, reason = admit_memory_candidate(agent_id, candidate, self._source_verifier)
        if not valid:
            return {"status": "rejected", "candidate_id": candidate.candidate_id,
                    "receipt_id": self._mint_receipt_id(), "reason_code": reason or "unspecified"}
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
