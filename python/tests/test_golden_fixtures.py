from __future__ import annotations

import json
from pathlib import Path

import pytest

from deepstrike.kernel.canonical import (
  CanonicalKernel,
  CanonicalPrepared,
)


FIXTURE_DIR = Path(__file__).parents[2] / "tests/fixtures/kernel-wire"


def _drive_fixture(name: str) -> CanonicalKernel:
  fixture = json.loads((FIXTURE_DIR / name).read_text(encoding="utf-8"))
  kernel = CanonicalKernel()
  for index, link in enumerate(fixture["links"]):
    assert "abi_version" not in link["envelope"]
    prepared = kernel.prepare(json.dumps(link["envelope"], separators=(",", ":")))
    assert isinstance(prepared, CanonicalPrepared)
    assert prepared.step_seq == index
    assert json.loads(prepared.planned_step_json) == link["step"]
    committed = kernel.commit(prepared.prepare_token, prepared.record_digest)
    assert committed.step_seq == index
    assert committed.record_digest == prepared.record_digest
  return kernel


@pytest.mark.parametrize(
  ("fixture_name", "expected_lifecycle"),
  [
    ("golden_lifecycle_agent_root.json", "running"),
    ("golden_lifecycle_workflow_root.json", "completed"),
  ],
)
def test_canonical_lifecycle_golden_fixture(
  fixture_name: str,
  expected_lifecycle: str,
) -> None:
  assert _drive_fixture(fixture_name).lifecycle() == expected_lifecycle
