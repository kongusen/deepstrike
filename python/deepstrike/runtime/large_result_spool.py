from __future__ import annotations

import hashlib
from pathlib import Path


class LargeResultSpool:
  """Layer-1 large result spool: kernel decides, SDK writes full output to disk."""

  def __init__(self, spool_dir: str = ".spool") -> None:
    self._spool_dir = Path(spool_dir)

  async def persist_output(self, call_id: str, content: str) -> str:
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]
    self._spool_dir.mkdir(parents=True, exist_ok=True)
    path = self._spool_dir / f"{call_id}-{digest}.txt"
    path.write_text(content, encoding="utf-8")
    return str(path)
