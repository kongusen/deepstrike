"""M2 bridge: how a host's `memory_candidate_result` becomes the kernel effect input.

The commit is replaced so the test sees the resolution the bridge built, without driving a full
run. The cases match the node and wasm bridge tests one for one.
"""
from __future__ import annotations

from typing import Any

import pytest

from deepstrike.runtime.kernel_journal import InMemoryKernelJournal


async def resolve(event: dict[str, Any]) -> dict[str, Any]:
  from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime

  runtime = CanonicalRunnerRuntime(object(), InMemoryKernelJournal(), "op-candidate-bridge",
                                   max_context_tokens=100_000)
  runtime._started = True
  captured: list[dict[str, Any]] = []

  async def commit(input: dict[str, Any]) -> None:
    captured.append(input)
    return None

  runtime._commit = commit  # type: ignore[method-assign]
  await runtime.apply_host_event({"effect_id": "effect-7", **event})
  assert captured, "the bridge produced no resolution"
  return captured[0]


@pytest.mark.asyncio
async def test_pending_resolves_with_a_candidate_and_no_record_receipt():
  input = await resolve({"kind": "memory_candidate_result", "status": "pending",
                         "candidate_id": "cand-1", "receipt_id": "rcpt-1"})
  assert input["kind"] == "resolve_effect"
  assert input["effect_id"] == "effect-7"
  assert input["outcome"]["status"] == "succeeded"
  result = input["outcome"]["result"]
  assert result["kind"] == "memory_persisted"
  assert result["candidate"]["candidate_id"] == "cand-1"
  assert result["candidate"]["receipt_id"] == "rcpt-1"
  assert "receipt" not in result


@pytest.mark.asyncio
async def test_accepted_resolves_like_a_persisted_record():
  input = await resolve({"kind": "memory_candidate_result", "status": "accepted",
                         "record_ref": "rec-3", "digest": "sha256:" + "0" * 64})
  result = input["outcome"]["result"]
  assert result["kind"] == "memory_persisted"
  assert result["receipt"]["record_ref"] == "rec-3"
  assert "candidate" not in result


@pytest.mark.asyncio
async def test_rejected_fails_non_retryably_and_names_the_reason():
  input = await resolve({"kind": "memory_candidate_result", "status": "rejected",
                         "reason_code": "source_unverifiable"})
  failure = input["outcome"]["failure"]
  assert input["outcome"]["status"] == "failed"
  assert failure["kind"] == "storage_unavailable"
  assert failure["retryable"] is False
  assert "source_unverifiable" in failure["message"]


@pytest.mark.asyncio
async def test_a_transport_error_fails_retryably():
  input = await resolve({"kind": "memory_candidate_result", "error": "sink timeout"})
  assert input["outcome"]["status"] == "failed"
  assert input["outcome"]["failure"]["retryable"] is True


@pytest.mark.asyncio
async def test_a_pending_answer_naming_no_candidate_is_a_non_retryable_failure():
  input = await resolve({"kind": "memory_candidate_result", "status": "pending", "receipt_id": "rcpt-1"})
  assert input["outcome"]["status"] == "failed"
  assert input["outcome"]["failure"]["retryable"] is False
