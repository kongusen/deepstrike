"""Shared candidate receipt mappings, capturing one terminal effect resolution per event.

Kernel idempotency and persistence are outside this bridge test.
"""
from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from deepstrike.runtime.kernel_journal import InMemoryKernelJournal


FIXTURE = json.loads((Path(__file__).resolve().parents[2]
                      / "tests/fixtures/abi/memory_candidate_receipts.json").read_text())


async def resolve(event: dict[str, Any]) -> dict[str, Any]:
  from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime

  runtime = CanonicalRunnerRuntime(object(), InMemoryKernelJournal(), "op-candidate-bridge",
                                   max_context_tokens=100_000,
                                   memory_binding_id=FIXTURE["binding_id"])
  runtime._started = True
  captured: list[dict[str, Any]] = []

  async def commit(input: dict[str, Any]) -> None:
    captured.append(input)
    return None

  runtime._commit = commit  # type: ignore[method-assign]
  await runtime.apply_host_event({**event, "effect_id": FIXTURE["effect_id"]})
  assert len(captured) == 1, "each event must produce exactly one terminal resolution"
  return captured[0]


@pytest.mark.asyncio
@pytest.mark.parametrize("case", FIXTURE["cases"], ids=lambda case: case["id"])
async def test_shared_candidate_receipt(case: dict[str, Any]):
  input = await resolve(case["event"])
  expected = case["expected"]
  # Exact equality also forbids a result containing both candidate and receipt arms.
  assert input == {
    "kind": "resolve_effect",
    "effect_id": FIXTURE["effect_id"],
    "outcome": expected,
  }
