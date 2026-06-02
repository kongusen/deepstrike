from __future__ import annotations

import asyncio
import hashlib
import time
from pathlib import Path


class LargeResultSpool:
  """Layer-1 large result spool: kernel decides, SDK writes full output to disk."""

  def __init__(self, spool_dir: str = ".spool", max_age_seconds: int | None = None) -> None:
    self._spool_dir = Path(spool_dir)
    self._max_age_seconds = max_age_seconds
    self._active_writes: dict[str, asyncio.Lock] = {}

  async def persist_output(self, call_id: str, content: str) -> str:
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    self._spool_dir.mkdir(parents=True, exist_ok=True)
    path = self._spool_dir / f"{call_id}-{digest}.txt"
    path_str = str(path)

    if path_str not in self._active_writes:
      self._active_writes[path_str] = asyncio.Lock()

    async with self._active_writes[path_str]:
      def _write():
        path.write_text(content, encoding="utf-8")
      await asyncio.get_running_loop().run_in_executor(None, _write)

    return path_str

  async def read_spooled_result(self, spool_ref: str) -> str:
    path = Path(spool_ref)
    if not path.is_file():
      raise FileNotFoundError(f"Spooled result not found: {spool_ref}")

    def _read():
      return path.read_text(encoding="utf-8")
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
