from __future__ import annotations

import asyncio
import hashlib
import os
import time
import uuid
from pathlib import Path


class LargeResultSpool:
  """Layer-1 large result spool: kernel decides, SDK writes full output to disk."""

  def __init__(self, spool_dir: str = ".spool", max_age_seconds: int | None = None) -> None:
    self._spool_dir = Path(spool_dir)
    self._max_age_seconds = max_age_seconds
    self._active_writes: dict[str, asyncio.Lock] = {}

  @staticmethod
  def _call_key(session_id: str, call_id: str) -> str:
    # Session-scoped: the spool dir is shared across sessions and outlives runs, while vendor
    # call ids can be index-style ("call_0") and repeat — an unscoped key lets read_result in
    # one session fetch another session's spooled output.
    return hashlib.sha256(f"{session_id}\x00{call_id}".encode("utf-8")).hexdigest()[:32]

  async def persist_output(self, session_id: str, call_id: str, content: str) -> str:
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    call_key = self._call_key(session_id, call_id)
    self._spool_dir.mkdir(parents=True, exist_ok=True)
    path = self._spool_dir / f"{call_key}-{digest}.txt"
    path_str = str(path)

    if path_str not in self._active_writes:
      self._active_writes[path_str] = asyncio.Lock()

    async with self._active_writes[path_str]:
      def _write():
        temp = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
        try:
          with temp.open("x", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
          os.replace(temp, path)
        finally:
          temp.unlink(missing_ok=True)
      await asyncio.get_running_loop().run_in_executor(None, _write)

    return path_str

  def _payload_path(self, session_id: str, payload_ref: str) -> Path:
    # PayloadRef is an opaque locator — hash the session-scoped value instead of treating it as a path.
    key = hashlib.sha256(f"{session_id}\x00{payload_ref}".encode("utf-8")).hexdigest()
    return self._spool_dir / f"payload-{key}.txt"

  async def persist_payload(
    self, session_id: str, call_id: str, payload_ref: str, content: str,
  ) -> None:
    """Persist under the host-issued opaque locator; retain the call-id index for audit tooling."""
    await asyncio.gather(
      self.persist_output(session_id, call_id, content),
      self._write_exact_path(self._payload_path(session_id, payload_ref), content),
    )

  async def _write_exact_path(self, path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path_str = str(path)
    if path_str not in self._active_writes:
      self._active_writes[path_str] = asyncio.Lock()

    async with self._active_writes[path_str]:
      def _write():
        temp = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
        try:
          with temp.open("x", encoding="utf-8") as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
          os.replace(temp, path)
        finally:
          temp.unlink(missing_ok=True)
      await asyncio.get_running_loop().run_in_executor(None, _write)

  async def load_payload(self, session_id: str, payload_ref: str) -> str | None:
    """Resolve an ABI-v3 payload locator without exposing the backing filesystem path."""
    path = self._payload_path(session_id, payload_ref)

    def _read() -> str | None:
      try:
        return path.read_text(encoding="utf-8")
      except FileNotFoundError:
        return None

    return await asyncio.get_running_loop().run_in_executor(None, _read)

  async def read_spooled_result(self, spool_ref: str) -> str:
    path = Path(spool_ref)
    if not path.is_file():
      raise FileNotFoundError(f"Spooled result not found: {spool_ref}")

    def _read():
      return path.read_text(encoding="utf-8")
    return await asyncio.get_running_loop().run_in_executor(None, _read)

  async def find_by_call_id(self, session_id: str, call_id: str) -> str | None:
    """O7: locate a spooled output by the tool call's id (the ``read_result`` meta-tool only
    knows ``call_id``, not the content-hashed file name ``persist_output`` chose). Scans the
    spool directory for the hashed session-scoped call-key prefix; returns ``None`` if nothing
    was ever spooled for that call."""
    if not self._spool_dir.is_dir():
      return None

    def _find() -> Path | None:
      call_key = self._call_key(session_id, call_id)
      matches = sorted(self._spool_dir.glob(f"{call_key}-*.txt"))
      return matches[0] if matches else None

    match = await asyncio.get_running_loop().run_in_executor(None, _find)
    if match is None:
      return None

    def _read():
      return match.read_text(encoding="utf-8")
    return await asyncio.get_running_loop().run_in_executor(None, _read)

  async def cleanup(self, max_age_seconds: int | None = None) -> int:
    limit = max_age_seconds if max_age_seconds is not None else self._max_age_seconds
    if limit is None:
      limit = 7 * 24 * 60 * 60  # 7 days

    if not self._spool_dir.is_dir():
      return 0

    count = 0
    now = time.time()

    def _do_cleanup():
      nonlocal count
      for f in self._spool_dir.glob("*.txt"):
        try:
          stat = f.stat()
          if now - stat.st_mtime > limit:
            f.unlink()
            count += 1
        except Exception:
          pass

    await asyncio.get_running_loop().run_in_executor(None, _do_cleanup)
    return count
