"""§22.13 · the kernel's memory authority for a host with no live operation.

Inside a run, every memory decision is a kernel transition (``admit_memory_write``, and the
``memory_recalled`` / ``promotion_suggested`` facts a query resolution journals). With no run — an
explicit ``remember``, a session extract after the run ended — the host asks the same kernel
functions statelessly. The SDK keeps no copy of the validation rule and never computes a recall
count itself.
"""

from __future__ import annotations

import json
from typing import Any


def _ask(request: dict[str, Any]) -> dict[str, Any]:
  from deepstrike._kernel import memory_authority_json

  return json.loads(memory_authority_json(json.dumps(request)))


def kernel_memory_policy_wire(flat: dict[str, Any] | None) -> dict[str, Any] | None:
  """The flat snake_case policy as the kernel's canonical wire carries it (``u64`` as a string)."""
  if not flat:
    return None
  wire = dict(flat)
  if wire.get("promotion_recall_threshold") is not None:
    wire["promotion_recall_threshold"] = str(int(wire["promotion_recall_threshold"]))
  return wire


def check_memory_write(policy: dict[str, Any] | None, name: str, content: str) -> str | None:
  """Would the kernel admit this write? Returns the refusal reason, or ``None`` when admitted."""
  answer = _ask({
    "op": "check_write",
    **({"policy": policy} if policy else {}),
    "name": name,
    "content_bytes": len(content.encode("utf-8")),
  })
  return None if answer.get("admitted") is True else str(answer.get("error") or "memory write refused")


def derive_memory_recall(
  policy: dict[str, Any] | None, recalled_at: int, hits: list[Any],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
  """Derive one recall's lifecycle from the counts the store reported it held.

  Returns ``(recalls, promotions)`` as the kernel answered them.
  """
  if not hits:
    return [], []
  answer = _ask({
    "op": "derive_recall",
    **({"policy": policy} if policy else {}),
    "recalled_at": max(0, int(recalled_at)),
    "recalls": [{
      "record_id": hit.record.record_id,
      "recall_count": max(0, int(hit.record.recall_count or 0)),
      **({"pinned": True} if hit.record.pinned else {}),
    } for hit in hits],
  })
  return list(answer.get("recalls") or []), list(answer.get("promotions") or [])
