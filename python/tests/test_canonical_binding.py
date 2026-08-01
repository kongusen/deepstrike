from __future__ import annotations

import json
from pathlib import Path

from deepstrike import _kernel
from deepstrike import kernel as kernel_facade
from deepstrike.kernel.canonical import (
  KERNEL_ABI_VERSION,
  CanonicalKernel,
  CanonicalPrepared,
  CanonicalRejected,
)


FIXTURE = json.loads(
  (Path(__file__).parents[2] / "tests/fixtures/kernel-wire/golden_lifecycle_agent_root.json")
  .read_text(encoding="utf-8")
)


def test_canonical_binding_passes_through_core_record_bytes_and_digest() -> None:
  assert KERNEL_ABI_VERSION == 3
  kernel = CanonicalKernel()
  prepared = kernel.prepare(json.dumps(FIXTURE["links"][0]["envelope"], separators=(",", ":")))
  assert isinstance(prepared, CanonicalPrepared)
  assert prepared.step_seq == 0
  assert prepared.record_digest == FIXTURE["genesis_digest"]
  assert prepared.record_bytes.decode() == json.dumps(
    FIXTURE["links"][0]["record"], separators=(",", ":")
  )

  committed = kernel.commit(prepared.prepare_token, prepared.record_digest)
  assert committed.step_seq == 0
  assert committed.record_digest == FIXTURE["genesis_digest"]
  assert kernel.lifecycle() == "configured"
  assert not hasattr(kernel, "step")

  replayed = kernel.prepare(json.dumps(FIXTURE["links"][0]["envelope"], separators=(",", ":")))
  assert replayed.status == "replayed"
  assert replayed.record_digest == FIXTURE["genesis_digest"]

  rebuilt = CanonicalKernel()
  restore_cost = rebuilt.restore(None, [prepared.record_bytes])
  assert restore_cost.records_before_checkpoint == 1
  assert restore_cost.records_after_checkpoint == 0
  assert rebuilt.lifecycle() == "configured"


def test_canonical_binding_rejects_strictly_and_restores_in_place() -> None:
  kernel = CanonicalKernel()
  rejected = kernel.prepare("{")
  assert isinstance(rejected, CanonicalRejected)
  assert rejected.code == "malformed_envelope"

  prepared = kernel.prepare(json.dumps(FIXTURE["links"][0]["envelope"], separators=(",", ":")))
  assert isinstance(prepared, CanonicalPrepared)
  kernel.commit(prepared.prepare_token, prepared.record_digest)
  checkpoint = kernel.checkpoint_candidate()
  identity = kernel
  kernel.restore(checkpoint.checkpoint_bytes, [])
  assert kernel is identity
  assert kernel.lifecycle() == "configured"


def test_native_binding_does_not_export_legacy_direct_step() -> None:
  assert not hasattr(kernel_facade, "KernelRuntime")
  assert not hasattr(_kernel, "KernelRuntime")
  assert not hasattr(_kernel._CanonicalKernel, "step")
  source = (
    Path(__file__).parents[2] / "crates/deepstrike-py/src/lib.rs"
  ).read_text(encoding="utf-8")
  assert "fn step(" not in source
  assert "struct KernelRuntime" not in source
