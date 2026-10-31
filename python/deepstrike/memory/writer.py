"""The host's side of a memory write, in one place.

The runner decides which path a write takes (the store, or the candidate sink for review); the
writer decides whether the write may happen and what its answer names: lease admission and the
store for a direct write, the sink and its receipt contract for a reviewed one.
"""
from __future__ import annotations

import hashlib
import time
from typing import Any, Callable

from .candidate_sink import MemoryCandidate, MemoryCandidateReceipt
from .protocols import MemoryRecord


class MemoryWriter:
  """Lease, store and candidate-sink receipt contract for every host memory write.

  Only direct store writes consume a lease; the sink's review decides what a candidate costs.
  """

  def __init__(self, *, store: Any | None = None, lifecycle: Any | None = None, sink: Any | None = None,
               now: Callable[[], int] | None = None) -> None:
    self._store = store
    self._lifecycle = lifecycle
    self._sink = sink
    self._now = now or (lambda: int(time.time() * 1000))

  async def persist(self, agent_id: str, record: MemoryRecord, write_id: str) -> dict[str, str]:
    """Write ``record`` to the store and return ``{record_ref, digest}``.

    ``write_id`` names the write to the lease, so a redelivered write spends the budget once.
    Raises when there is no store or the lease refuses the write.
    """
    if self._store is None:
      raise RuntimeError("memory persistence requires memory_store")
    if self._lifecycle is not None:
      admission = await self._lifecycle.consume_write(agent_id, record.scope, write_id, self._now())
      if not admission.get("valid"):
        raise RuntimeError(f"memory lease rejected: {admission.get('reason_code')}")
    await self._store.put(agent_id, record)
    return {
      "record_ref": record.record_id,
      "digest": "sha256:" + hashlib.sha256(record.content.encode("utf-8")).hexdigest(),
    }

  async def propose(self, agent_id: str, candidate: MemoryCandidate) -> MemoryCandidateReceipt:
    """Hand ``candidate`` to the sink and check its answer names this candidate (spec §2.3).

    A sink that raises has given no answer, and the same candidate may be redelivered.
    """
    if self._sink is None:
      raise RuntimeError("memory candidate writes require memory_candidate_sink")
    receipt = await self._sink.submit(agent_id, candidate)
    candidate_id = candidate.candidate_id
    if receipt.get("candidate_id") != candidate_id or not isinstance(receipt.get("receipt_id"), str) or not receipt["receipt_id"]:
      raise ValueError(f"memory candidate sink answered without a receipt for {candidate_id}")
    if receipt.get("status") not in ("pending", "accepted", "rejected"):
      raise ValueError(f"memory candidate sink answered with unknown status {receipt.get('status')}")
    if receipt["status"] == "accepted" and (not isinstance(receipt.get("record_ref"), str) or not receipt["record_ref"] or not isinstance(receipt.get("digest"), str) or not receipt["digest"]):
      raise ValueError("memory candidate sink accepted a write without record_ref/digest")
    if receipt["status"] == "rejected" and (not isinstance(receipt.get("reason_code"), str) or not receipt["reason_code"]):
      raise ValueError("memory candidate sink rejected a write without reason_code")
    return receipt
