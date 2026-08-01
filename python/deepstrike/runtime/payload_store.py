from __future__ import annotations

import asyncio
import hashlib
import os
import time
import uuid
from pathlib import Path


class PayloadStore:
  """File-backed storage for canonical opaque payload locators."""

  def __init__(self, storage_dir: str = ".payloads", max_age_seconds: int | None = None) -> None:
    self._storage_dir = Path(storage_dir)
    self._max_age_seconds = max_age_seconds
    self._active_writes: dict[str, tuple[asyncio.Lock, int]] = {}

  def _payload_path(self, session_id: str, payload_ref: str) -> Path:
    key = hashlib.sha256(f"{session_id}\x00{payload_ref}".encode()).hexdigest()
    return self._storage_dir / f"{key}.payload"

  async def persist_payload(self, session_id: str, payload_ref: str, content: str) -> None:
    path = self._payload_path(session_id, payload_ref)
    path.parent.mkdir(parents=True, exist_ok=True)
    key = str(path)
    entry = self._active_writes.get(key)
    if entry is None:
      lock = asyncio.Lock()
      self._active_writes[key] = (lock, 1)
    else:
      lock, users = entry
      self._active_writes[key] = (lock, users + 1)

    try:
      async with lock:
        def _write() -> None:
          temporary = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
          try:
            with temporary.open("x", encoding="utf-8") as handle:
              handle.write(content)
              handle.flush()
              os.fsync(handle.fileno())
            os.replace(temporary, path)
          finally:
            temporary.unlink(missing_ok=True)

        await asyncio.get_running_loop().run_in_executor(None, _write)
    finally:
      current = self._active_writes.get(key)
      if current is not None and current[0] is lock:
        if current[1] == 1:
          self._active_writes.pop(key, None)
        else:
          self._active_writes[key] = (lock, current[1] - 1)

  async def load_payload(self, session_id: str, payload_ref: str) -> str | None:
    path = self._payload_path(session_id, payload_ref)

    def _read() -> str | None:
      try:
        return path.read_text(encoding="utf-8")
      except FileNotFoundError:
        return None

    return await asyncio.get_running_loop().run_in_executor(None, _read)

  async def cleanup(self, max_age_seconds: int | None = None) -> int:
    limit = max_age_seconds if max_age_seconds is not None else self._max_age_seconds
    if limit is None:
      limit = 7 * 24 * 60 * 60
    if not self._storage_dir.is_dir():
      return 0

    now = time.time()

    def _remove_expired() -> int:
      removed = 0
      for path in self._storage_dir.glob("*.payload"):
        if now - path.stat().st_mtime > limit:
          path.unlink()
          removed += 1
      return removed

    return await asyncio.get_running_loop().run_in_executor(None, _remove_expired)
