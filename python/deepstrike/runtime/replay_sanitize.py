"""UTF-8-safe text helpers for session replay (defense in depth).

When running against SDK builds where renderer/compression slice by raw byte
index (pre-fix), replaying long CJK-heavy ``llm_completed`` content increases
panic risk during context render. Truncate at char boundaries before preload.
"""

from __future__ import annotations

# Soft cap — only snip when content is very large; keeps replay faithful for normal turns.
REPLAY_CONTENT_MAX_BYTES = 32_768


def truncate_bytes_at_char_boundary(text: str, max_bytes: int) -> str:
  data = text.encode("utf-8")
  if len(data) <= max_bytes:
    return text
  end = max_bytes
  while end > 0:
    try:
      return data[:end].decode("utf-8")
    except UnicodeDecodeError:
      end -= 1
  return ""


def sanitize_replay_text(text: str, max_bytes: int | None = None) -> str:
  if not text:
    return text
  if max_bytes is None or max_bytes == 0:
    max_bytes = REPLAY_CONTENT_MAX_BYTES
  safe = text.encode("utf-8", errors="replace").decode("utf-8")
  if len(safe.encode("utf-8")) <= max_bytes:
    return safe
  prefix = truncate_bytes_at_char_boundary(safe, max_bytes)
  return f"{prefix}… [replay truncated]"
