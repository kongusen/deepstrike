"""Durable lease, budget, and lifecycle event state."""
from __future__ import annotations

import copy
import json
import os
import time
import uuid
from pathlib import Path
from dataclasses import asdict
from typing import Any, Awaitable, Callable

from .lifecycle import MemoryDeletionReceipt, MemoryLease, MemoryLifecycleEvent, MemoryLifecycleWriteResult, verify_memory_lease, consume_memory_lease_write, revoke_memory_lease
from .protocols import MemoryScope


def _lease(raw: dict[str, Any]) -> MemoryLease:
    return MemoryLease(lease_id=raw["lease_id"], agent_id=raw["agent_id"], scope=MemoryScope(**raw["scope"]), issued_at=raw["issued_at"], expires_at=raw["expires_at"], max_writes=raw["max_writes"], writes_used=raw.get("writes_used", 0), revoked_at=raw.get("revoked_at"))


class FileMemoryLifecycleStore:
    def __init__(self, path: str | os.PathLike[str], *, now: Any = None, lock_retry_ms: int = 10, lock_timeout_ms: int = 30_000,
                 delete_physical: Callable[[str, MemoryScope, str], Awaitable[bool]] | None = None) -> None:
        self._path = Path(path)
        self._lock_path = Path(f"{path}.lock")
        self._now = now or (lambda: int(time.time() * 1000))
        self._retry = lock_retry_ms / 1000
        self._timeout = lock_timeout_ms / 1000
        self._delete_physical = delete_physical

    async def issue_lease(self, lease: MemoryLease) -> MemoryLease:
        with self._lock():
            state = self._read(); existing = next((x for x in state["leases"] if x["lease_id"] == lease.lease_id), None)
            if existing:
                if existing != asdict(lease): raise ValueError("memory lease conflict")
                return copy.deepcopy(lease)
            state["leases"].append(asdict(lease)); self._append(state, "lease_issued", lease, None, lease.issued_at, {"max_writes": lease.max_writes, "expires_at": lease.expires_at}); self._write(state)
            return copy.deepcopy(lease)

    async def get_lease(self, agent_id: str, scope: MemoryScope) -> MemoryLease | None:
        with self._lock():
            raw = next((x for x in self._read()["leases"] if x["agent_id"] == agent_id and x["scope"] == asdict(scope)), None)
            return _lease(raw) if raw else None

    async def renew_lease(self, agent_id: str, lease_id: str, expires_at: int, renewed_at: int) -> MemoryLease:
        with self._lock():
            state = self._read(); raw = self._require(state, agent_id, lease_id); lease = _lease(raw)
            if expires_at <= renewed_at or expires_at < lease.expires_at: raise ValueError("invalid memory lease renewal")
            updated = MemoryLease(lease.lease_id, lease.agent_id, lease.scope, lease.issued_at, expires_at, lease.max_writes, lease.writes_used, lease.revoked_at)
            state["leases"][state["leases"].index(raw)] = asdict(updated); self._append(state, "lease_renewed", updated, None, renewed_at, {"expires_at": expires_at}); self._write(state)
            return copy.deepcopy(updated)

    async def consume_write(self, agent_id: str, scope: MemoryScope, operation_id: str, now_ms: int) -> MemoryLifecycleWriteResult:
        with self._lock():
            state = self._read(); key = f"{agent_id}:{operation_id}"
            old = next((x["result"] for x in state["consumed"] if x["key"] == key), None)
            if old is not None: return copy.deepcopy(old)
            raw = next((x for x in state["leases"] if x["agent_id"] == agent_id and x["scope"] == asdict(scope)), None)
            verdict = verify_memory_lease(_lease(raw) if raw else None, agent_id, scope, now_ms)
            if not verdict[0]:
                result = {"valid": False, "reason_code": verdict[1]}; state["consumed"].append({"key": key, "result": result}); self._write(state); return result
            updated = consume_memory_lease_write(_lease(raw)); state["leases"][state["leases"].index(raw)] = asdict(updated)
            event = self._append(state, "lease_write_consumed", updated, operation_id, now_ms, {"writes_used": updated.writes_used})
            result = {"valid": True, "lease": asdict(updated), "event_id": event["event_id"]}; state["consumed"].append({"key": key, "result": result}); self._write(state); return copy.deepcopy(result)

    async def revoke_lease(self, agent_id: str, lease_id: str, revoked_at: int) -> MemoryLease:
        with self._lock():
            state = self._read(); raw = self._require(state, agent_id, lease_id); lease = _lease(raw)
            if lease.revoked_at is not None: return lease
            updated = revoke_memory_lease(lease, revoked_at); state["leases"][state["leases"].index(raw)] = asdict(updated); self._append(state, "lease_revoked", updated, None, revoked_at, {"revoked_at": revoked_at}); self._write(state); return updated

    async def delete_record(self, agent_id: str, scope: MemoryScope, record_id: str, deletion_id: str, prior_digest: str | None = None) -> MemoryDeletionReceipt:
        """Request-then-apply deletion: the request is durable before the physical delete runs,
        so a crash in between is retried with the same deletion_id."""
        request = {"deletion_id": deletion_id, "record_id": record_id, **({"prior_digest": prior_digest} if prior_digest else {})}
        marker = MemoryLease(f"deletion:{deletion_id}", agent_id, scope, 0, 0, 0)
        with self._lock():
            state = self._read()
            existing = self._find_deleted(state, agent_id, deletion_id)
            if existing is not None: return copy.deepcopy(existing)
            requested = any(e["type"] == "deletion_requested" and e["agent_id"] == agent_id and e.get("operation_id") == deletion_id for e in state["events"])
            if not requested:
                self._append(state, "deletion_requested", marker, deletion_id, self._now(), request); self._write(state)
        applied = True if self._delete_physical is None else await self._delete_physical(agent_id, scope, record_id)
        with self._lock():
            state = self._read()
            existing = self._find_deleted(state, agent_id, deletion_id)
            if existing is not None: return copy.deepcopy(existing)
            event = self._append(state, "deletion_applied" if applied else "deletion_noop", marker, deletion_id, self._now(), request)
            receipt = {"deletion_id": deletion_id, "agent_id": agent_id, "scope": asdict(scope), "record_id": record_id,
                       **({"prior_digest": prior_digest} if prior_digest else {}),
                       "status": "applied" if applied else "noop", "event_sequence": event["sequence"]}
            state["deleted"].append(receipt); self._write(state)
            return copy.deepcopy(receipt)

    async def deleted(self, agent_id: str | None = None) -> list[MemoryDeletionReceipt]:
        with self._lock():
            return [copy.deepcopy(x) for x in self._read()["deleted"] if agent_id is None or x["agent_id"] == agent_id]

    async def events(self) -> list[MemoryLifecycleEvent]:
        with self._lock(): return copy.deepcopy(self._read()["events"])

    @staticmethod
    def _find_deleted(state: dict[str, Any], agent_id: str, deletion_id: str) -> dict[str, Any] | None:
        return next((x for x in state["deleted"] if x["agent_id"] == agent_id and x["deletion_id"] == deletion_id), None)

    def _require(self, state: dict[str, Any], agent_id: str, lease_id: str) -> dict[str, Any]:
        raw = next((x for x in state["leases"] if x["lease_id"] == lease_id and x["agent_id"] == agent_id), None)
        if raw is None: raise ValueError("memory lease not found")
        return raw

    def _append(self, state: dict[str, Any], kind: str, lease: MemoryLease, operation_id: str | None, occurred_at: int, payload: dict[str, Any]) -> dict[str, Any]:
        event = {"event_id": f"{kind}:{lease.lease_id}:{operation_id or len(state['events'])}", "sequence": len(state["events"]), "type": kind, "agent_id": lease.agent_id, "lease_id": lease.lease_id, "scope": asdict(lease.scope), "occurred_at": occurred_at, "payload": payload}
        if operation_id: event["operation_id"] = operation_id
        state["events"].append(event); return event

    def _read(self) -> dict[str, Any]:
        try: raw = json.loads(self._path.read_text())
        except FileNotFoundError: return {"version": 1, "leases": [], "consumed": [], "deleted": [], "events": []}
        if raw.get("version") != 1 or not all(isinstance(raw.get(k), list) for k in ("leases", "consumed", "events")) or not isinstance(raw.get("deleted", []), list): raise ValueError(f"invalid memory lifecycle snapshot: {self._path}")
        raw.setdefault("deleted", [])
        return raw

    def _write(self, state: dict[str, Any]) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True); temp = self._path.with_name(f"{self._path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
        try:
            with temp.open("w") as handle: json.dump(state, handle, separators=(",", ":")); handle.flush(); os.fsync(handle.fileno())
            os.replace(temp, self._path)
        finally: temp.unlink(missing_ok=True)

    class _Lock:
        def __init__(self, owner: "FileMemoryLifecycleStore") -> None: self.owner = owner
        def __enter__(self):
            started = time.monotonic(); self.owner._path.parent.mkdir(parents=True, exist_ok=True)
            while True:
                try: self.owner._lock_path.mkdir(); return self
                except FileExistsError:
                    if time.monotonic() - started >= self.owner._timeout: raise TimeoutError("timed out acquiring memory lifecycle lock")
                    try:
                        if time.time() - self.owner._lock_path.stat().st_mtime > self.owner._timeout: self.owner._lock_path.rmdir(); continue
                    except FileNotFoundError: continue
                    time.sleep(self.owner._retry)
        def __exit__(self, *_): self.owner._lock_path.rmdir()
    def _lock(self): return self._Lock(self)
