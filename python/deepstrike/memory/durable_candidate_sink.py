"""Durable file-backed memory candidate ledger."""
from __future__ import annotations

import asyncio
import copy
import json
import os
import time
import uuid
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Literal

from .candidate_sink import (
    MemoryCandidate,
    MemoryCandidateReceipt,
    MemoryCandidateSource,
    memory_candidate_payload_digest,
)
from .attestation import MemoryCandidateAttestation, memory_candidate_attestation_digest, verify_memory_candidate_source
from .protocols import MemoryProvenance, MemoryRecord, MemoryScope

MemoryCandidateResolution = dict[str, str]


@dataclass
class MemoryCandidateLedgerEntry:
    agent_id: str
    candidate: MemoryCandidate
    payload_digest: str
    receipt: MemoryCandidateReceipt
    created_at: int
    updated_at: int


@dataclass
class MemoryCandidateOutboxEvent:
    event_id: str
    type: Literal["candidate_submitted", "candidate_accepted", "candidate_rejected"]
    agent_id: str
    candidate_id: str
    receipt: MemoryCandidateReceipt
    occurred_at: int


@dataclass
class MemoryCandidateVerificationAttempt:
    attempt_id: str
    agent_id: str
    candidate_id: str
    reason_code: str
    attestation_digest: str | None = None
    issuer: str | None = None
    key_id: str | None = None
    occurred_at: int = 0


def _now_ms() -> int:
    return int(time.time() * 1000)


def _record_from_dict(raw: dict[str, Any]) -> MemoryRecord:
    return MemoryRecord(
        record_id=raw["record_id"],
        scope=MemoryScope(**raw["scope"]),
        name=raw["name"], kind=raw["kind"], content=raw["content"], description=raw["description"],
        provenance=MemoryProvenance(**raw["provenance"]), created_at=raw["created_at"], updated_at=raw["updated_at"],
        last_recalled_at=raw.get("last_recalled_at"), recall_count=raw.get("recall_count", 0),
        confidence=raw.get("confidence", 1.0), links=list(raw.get("links", [])), pinned=raw.get("pinned", False),
        ttl_days=raw.get("ttl_days"),
    )


def _candidate_from_dict(raw: dict[str, Any]) -> MemoryCandidate:
    attestation = raw.get("attestation")
    return MemoryCandidate(raw["candidate_id"], raw["source"], _record_from_dict(raw["record"]),
                           MemoryCandidateAttestation(**attestation) if attestation else None)


class FileMemoryCandidateSink:
    """Atomic file-backed candidate sink with restart recovery and a durable outbox."""

    def __init__(self, path: str | os.PathLike[str], *, now: Any = None,
                 lock_retry_ms: int = 10, lock_timeout_ms: int = 30_000, source_verifier=None) -> None:
        self._path = Path(path)
        self._lock_path = Path(f"{path}.lock")
        self._now = now or _now_ms
        self._lock_retry = lock_retry_ms / 1000
        self._lock_timeout = lock_timeout_ms / 1000
        self._source_verifier = source_verifier
        self._local_lock = asyncio.Lock()

    async def submit(self, agent_id: str, candidate: MemoryCandidate) -> MemoryCandidateReceipt:
        async with self._local_lock:
            with self._lock():
                state = self._read()
                digest = memory_candidate_payload_digest(candidate.record)
                existing = next((entry for entry in state["entries"]
                                 if entry["agent_id"] == agent_id and entry["candidate"]["candidate_id"] == candidate.candidate_id), None)
                if existing is not None:
                    if existing["payload_digest"] == digest and existing["candidate"]["source"] == candidate.source:
                        return copy.deepcopy(existing["receipt"])
                    receipt: MemoryCandidateReceipt = {"status": "rejected", "candidate_id": candidate.candidate_id,
                                                       "receipt_id": self._mint_receipt(state), "reason_code": "candidate_conflict"}
                    self._outbox(state, "candidate_rejected", agent_id, candidate.candidate_id, receipt)
                    self._write(state)
                    return copy.deepcopy(receipt)
                valid, reason = verify_memory_candidate_source(agent_id, candidate, self._source_verifier)
                if not valid:
                    receipt: MemoryCandidateReceipt = {"status": "rejected", "candidate_id": candidate.candidate_id,
                                                       "receipt_id": self._mint_receipt(state), "reason_code": reason or "unspecified"}
                    state["verification_attempts"].append({"attempt_id": f"verification:{state['next_event']}",
                                                            "agent_id": agent_id, "candidate_id": candidate.candidate_id,
                                                            "reason_code": reason or "unspecified",
                                                            **({"attestation_digest": memory_candidate_attestation_digest(candidate.attestation),
                                                                "issuer": candidate.attestation.issuer, "key_id": candidate.attestation.key_id}
                                                               if candidate.attestation else {}),
                                                            "occurred_at": self._now()})
                    state["next_event"] += 1
                    self._write(state)
                    return copy.deepcopy(receipt)
                receipt = {"status": "pending", "candidate_id": candidate.candidate_id,
                           "receipt_id": self._mint_receipt(state)}
                stamp = self._now()
                state["entries"].append({"agent_id": agent_id, "candidate": asdict(copy.deepcopy(candidate)),
                                         "payload_digest": digest, "receipt": copy.deepcopy(receipt),
                                         "created_at": stamp, "updated_at": stamp})
                self._outbox(state, "candidate_submitted", agent_id, candidate.candidate_id, receipt)
                self._write(state)
                return copy.deepcopy(receipt)

    async def verification_attempts(self, agent_id: str | None = None) -> list[MemoryCandidateVerificationAttempt]:
        async with self._local_lock:
            with self._lock():
                return [MemoryCandidateVerificationAttempt(**copy.deepcopy(attempt)) for attempt in self._read()["verification_attempts"]
                        if agent_id is None or attempt["agent_id"] == agent_id]

    async def list(self, agent_id: str | None = None) -> list[MemoryCandidateLedgerEntry]:
        async with self._local_lock:
            with self._lock():
                return [self._entry(raw) for raw in self._read()["entries"]
                        if agent_id is None or raw["agent_id"] == agent_id]

    async def resolve(self, agent_id: str, candidate_id: str, resolution: MemoryCandidateResolution) -> MemoryCandidateReceipt:
        async with self._local_lock:
            with self._lock():
                state = self._read()
                entry = next((raw for raw in state["entries"]
                              if raw["agent_id"] == agent_id and raw["candidate"]["candidate_id"] == candidate_id), None)
                if entry is None:
                    raise ValueError(f"unknown memory candidate: {agent_id}/{candidate_id}")
                if entry["receipt"]["status"] != "pending":
                    return copy.deepcopy(entry["receipt"])
                if resolution.get("status") == "accepted":
                    receipt: MemoryCandidateReceipt = {"status": "accepted", "candidate_id": candidate_id,
                                                       "receipt_id": entry["receipt"]["receipt_id"],
                                                       "record_ref": resolution["record_ref"], "digest": resolution["digest"]}
                elif resolution.get("status") == "rejected":
                    receipt = {"status": "rejected", "candidate_id": candidate_id,
                               "receipt_id": entry["receipt"]["receipt_id"], "reason_code": resolution["reason_code"]}
                else:
                    raise ValueError("candidate resolution status must be accepted or rejected")
                entry["receipt"] = receipt
                entry["updated_at"] = self._now()
                self._outbox(state, "candidate_accepted" if receipt["status"] == "accepted" else "candidate_rejected",
                             agent_id, candidate_id, receipt)
                self._write(state)
                return copy.deepcopy(receipt)

    async def outbox(self, agent_id: str | None = None) -> list[MemoryCandidateOutboxEvent]:
        async with self._local_lock:
            with self._lock():
                return [MemoryCandidateOutboxEvent(**copy.deepcopy(raw)) for raw in self._read()["outbox"]
                        if agent_id is None or raw["agent_id"] == agent_id]

    async def ack_outbox(self, event_id: str) -> bool:
        async with self._local_lock:
            with self._lock():
                state = self._read()
                kept = [event for event in state["outbox"] if event["event_id"] != event_id]
                if len(kept) == len(state["outbox"]):
                    return False
                state["outbox"] = kept
                self._write(state)
                return True

    def _entry(self, raw: dict[str, Any]) -> MemoryCandidateLedgerEntry:
        return MemoryCandidateLedgerEntry(raw["agent_id"], _candidate_from_dict(raw["candidate"]), raw["payload_digest"],
                                          copy.deepcopy(raw["receipt"]), raw["created_at"], raw["updated_at"])

    def _mint_receipt(self, state: dict[str, Any]) -> str:
        receipt = f"receipt:{state['next_receipt']}"
        state["next_receipt"] += 1
        return receipt

    def _outbox(self, state: dict[str, Any], event_type: str, agent_id: str, candidate_id: str,
                receipt: MemoryCandidateReceipt) -> None:
        state["outbox"].append({"event_id": f"outbox:{state['next_event']}", "type": event_type,
                                "agent_id": agent_id, "candidate_id": candidate_id,
                                "receipt": copy.deepcopy(receipt), "occurred_at": self._now()})
        state["next_event"] += 1

    def _read(self) -> dict[str, Any]:
        try:
            raw = json.loads(self._path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return {"version": 1, "next_receipt": 1, "next_event": 1, "entries": [], "outbox": [], "verification_attempts": []}
        if (raw.get("version") != 1 or not isinstance(raw.get("next_receipt"), int) or raw["next_receipt"] < 1
                or not isinstance(raw.get("next_event"), int) or raw["next_event"] < 1
                or not isinstance(raw.get("entries"), list) or not isinstance(raw.get("outbox"), list)
                or not isinstance(raw.get("verification_attempts", []), list)):
            raise ValueError(f"invalid memory candidate ledger: {self._path}")
        raw.setdefault("verification_attempts", [])
        return raw

    def _write(self, state: dict[str, Any]) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self._path.with_name(f"{self._path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
        try:
            with temporary.open("w", encoding="utf-8") as handle:
                json.dump(state, handle, ensure_ascii=False, indent=2, separators=(",", ": "))
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(temporary, self._path)
            try:
                directory = os.open(self._path.parent, os.O_RDONLY)
                try:
                    os.fsync(directory)
                finally:
                    os.close(directory)
            except OSError:
                pass
        finally:
            temporary.unlink(missing_ok=True)

    class _Lock:
        def __init__(self, owner: "FileMemoryCandidateSink") -> None:
            self.owner = owner
        def __enter__(self) -> "FileMemoryCandidateSink._Lock":
            started = time.monotonic()
            self.owner._path.parent.mkdir(parents=True, exist_ok=True)
            while True:
                try:
                    self.owner._lock_path.mkdir()
                    return self
                except FileExistsError:
                    if time.monotonic() - started >= self.owner._lock_timeout:
                        raise TimeoutError(f"timed out acquiring memory candidate ledger lock: {self.owner._lock_path}")
                    try:
                        if time.time() - self.owner._lock_path.stat().st_mtime > self.owner._lock_timeout:
                            self.owner._lock_path.rmdir()
                            continue
                    except FileNotFoundError:
                        continue
                    time.sleep(self.owner._lock_retry)
        def __exit__(self, *_: object) -> None:
            self.owner._lock_path.rmdir()

    def _lock(self) -> "FileMemoryCandidateSink._Lock":
        return self._Lock(self)
