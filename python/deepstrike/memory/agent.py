from __future__ import annotations

from typing import Any


def memories_to_index(entries: list[Any]) -> list[dict[str, Any]]:
  out: list[dict[str, Any]] = []
  for entry in entries:
    meta = entry.metadata if hasattr(entry, "metadata") else (entry.get("metadata") if isinstance(entry, dict) else {})
    if not isinstance(meta, dict):
      meta = {}
    text = entry.text if hasattr(entry, "text") else (entry.get("text") if isinstance(entry, dict) else "")
    out.append({
      "name": str(meta.get("name") or text[:40]),
      "description": str(meta.get("description") or text[:120]),
      "kind": meta.get("kind"),
      "file": str(meta.get("file") or ""),
      "updated_at": int(meta.get("updated_at") or 0),
    })
  return out


async def select_memories(query: dict[str, Any], memory_index: list[dict[str, Any]]) -> dict[str, Any]:
  filter_out = set(query.get("already_surfaced") or []) | set(query.get("active_tools") or [])
  candidates = [entry for entry in memory_index if entry.get("name") not in filter_out]
  top_k = int(query.get("top_k") or 5)
  if not candidates:
    return {"selected_memory_ids": [], "selection_rationale": "No candidates after filtering"}
  selected = [str(entry.get("name") or "") for entry in candidates[:top_k]]
  return {
    "selected_memory_ids": selected,
    "selection_rationale": "Stub selector ranked index entries",
  }
