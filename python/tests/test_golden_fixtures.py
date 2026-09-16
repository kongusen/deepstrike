"""P7-S1 / F16: this suite consumes the ENTIRE tests/fixtures/kernel-wire directory via
listdir — a fixture that is neither executed nor charter-exempted below fails the sweep
test. The charter lists the only fixtures not executed here, each with the surface that
does enforce it. An empty charter is the default; every entry needs a reason.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from deepstrike.kernel.canonical import (
  CanonicalKernel,
  CanonicalPrepared,
  CanonicalRejected,
)


FIXTURE_DIR = Path(__file__).parents[2] / "tests/fixtures/kernel-wire"


def _read_fixture(name: str) -> Any:
  return json.loads((FIXTURE_DIR / name).read_text(encoding="utf-8"))


def _canon(value: Any) -> str:
  """Compact JSON matching serde/JSON.stringify byte shape (literal UTF-8)."""
  return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


NAMES = sorted(path.name for path in FIXTURE_DIR.iterdir() if path.name.endswith(".json"))


def _by_prefix(prefix: str) -> list[str]:
  return [name for name in NAMES if name.startswith(prefix)]


def _prepare_fault(kernel: CanonicalKernel, envelope: Any) -> tuple[str, str | None, str]:
  """Decode-stage deaths surface from the binding as malformed_envelope; policy violations
  from config resolution surface as invalid_config (binding.rs rejection_fault). Anything
  else (prepared/replayed/lifecycle faults) proves the bytes were accepted at the wire."""
  outcome = kernel.prepare(_canon(envelope))
  if isinstance(outcome, CanonicalRejected):
    return ("rejected", outcome.code, outcome.message)
  return (outcome.status, None, "")


# expect kind → marker substring guaranteed present in the rejection message.
REJECT_MARKERS = {
  "unknown_field": "unknown field",
  "unknown_variant": "unknown variant",
  "missing_field": "missing field",
  "invalid_scalar": "wire scalar rejected",
}

# Envelope-owned facts a business input must never repeat (mirror of core tests.rs).
BANNED_INPUT_KEYS = [
  "operation_id", "event_id", "now_ms", "observed_at_ms", "session_id",
  "parent_session_id", "agent_id", "memory_path", "path", "file_path", "spool_dir",
]

# Host-owned facts a configuration fixture must never carry (mirror of core config.rs).
BANNED_CONFIG_KEYS = [
  "memory_path", "spool_dir", "tokenizer", "host_effect_retry_attempts",
  "session_id", "api_key", "endpoint",
]


def _all_keys(value: Any, out: set[str]) -> None:
  if isinstance(value, list):
    for item in value:
      _all_keys(item, out)
  elif isinstance(value, dict):
    for key, child in value.items():
      out.add(key)
      _all_keys(child, out)


def _expected_lifecycle_for(fixture: Any) -> str:
  disposition = fixture["links"][-1]["step"]["disposition"]
  if disposition["kind"] != "terminal":
    return "running"
  terminal = disposition["terminal"]
  if terminal["kind"] == "cancelled":
    return "cancelled"
  if terminal["kind"] == "failed":
    return "failed"
  return "completed"


# -- family partitions ---------------------------------------------------------

LIFECYCLE = _by_prefix("golden_lifecycle_")
RECORD_CHAIN = _by_prefix("golden_record_chain")
RECORD_GENESIS = _by_prefix("golden_record_genesis")
CONFIG_RESOLVED = _by_prefix("golden_config_resolved")
CONFIG_REJECTS = _by_prefix("golden_config_reject_")
CONFIG_REJECTS_DEFAULT_LIMITS = [
  name for name in CONFIG_REJECTS if "bootstrap_limits" not in _read_fixture(name)
]
INPUTS = _by_prefix("input_")
ENVELOPE_REJECTS = [
  name
  for name in _by_prefix("reject_")
  if not name.startswith("reject_checkpoint_") and not name.startswith("reject_transaction_")
]

# Charter: fixtures NOT executed here, each with the enforcing surface.
# Families: prefix entries (ending in "_"); individual files: exact names without ".json".
CHARTER = {
  "reject_checkpoint_":
    "checkpoint blobs carry the §12 taxonomy, not the envelope decode boundary; enforced by "
    "core checkpoint::tests and by this SDK's restore rejection tests (test_canonical_binding.py)",
  "reject_transaction_":
    "§7.13 faults from a well-formed envelope the transaction refuses (checkpoint_required "
    "needs a full journal tail); enforced by core driver §12.3 tests",
  "golden_checkpoint_":
    "checkpoint candidate/rebase/restore snapshots are produced and pinned by core "
    "driver/tests.rs (J1: canonical bytes are core-owned); the SDK restore path is covered by "
    "test_canonical_binding.py restore-in-place assertions",
  "golden_record_canonical_bytes":
    "canonical byte vectors are core-owned (J1); asserted in core record.rs "
    "golden_canonical_bytes_vectors",
  "golden_record_transition":
    "its step is a synthetic record.rs helper step and a resolve_effect envelope is not "
    "drivable standalone (no pending effect exists by design); pinned by core record.rs "
    "golden_transition_record. The normalisation/chain-linkage surface is covered by the "
    "genesis/chain drives below",
  "golden_terminal_":
    "bare terminal wire shapes are owned by core terminal.rs; the SDK asserts terminals "
    "end-to-end via terminal_json() deep-equality in the lifecycle drive below",
  "golden_config_reject_limits_widen_bootstrap":
    "fixture requires fixture-supplied bootstrap limits; SDK bindings construct with defaults. "
    "Enforced by core config.rs with the fixture's limits",
}


def _charter_covers(name: str) -> bool:
  base = name.removesuffix(".json")
  return any(
    name.startswith(key) if key.endswith("_") else base == key for key in CHARTER
  )


# -- the sweep itself: nothing in the directory may go unclassified ------------


def test_every_fixture_is_executed_or_charter_exempted() -> None:
  executed = {
    *LIFECYCLE,
    *RECORD_CHAIN,
    *RECORD_GENESIS,
    *CONFIG_RESOLVED,
    *CONFIG_REJECTS_DEFAULT_LIMITS,
    *INPUTS,
    *ENVELOPE_REJECTS,
  }
  unclassified = [name for name in NAMES if name not in executed and not _charter_covers(name)]
  assert unclassified == []

  # every charter entry must still match something — a stale charter entry is drift
  stale = [
    key
    for key in CHARTER
    if not any(
      name.startswith(key) if key.endswith("_") else name.removesuffix(".json") == key
      for name in NAMES
    )
  ]
  assert stale == []

  # the charter must stay narrow: bootstrap_limits-carrying config rejects are the only
  # data-driven exemptions allowed beyond the list above
  for name in CONFIG_REJECTS:
    if name not in CONFIG_REJECTS_DEFAULT_LIMITS:
      assert name.removesuffix(".json") in CHARTER, f"{name}: unchartered bootstrap-limits exemption"


# -- golden lifecycles: drive every link, pin digests, records, terminal -------


@pytest.mark.parametrize("name", LIFECYCLE)
def test_lifecycle_fixture_drives_to_declared_digests(name: str) -> None:
  fixture = _read_fixture(name)
  kernel = CanonicalKernel()
  links = fixture["links"]

  for index, link in enumerate(links):
    assert "abi_version" not in link["envelope"]
    prepared = kernel.prepare(_canon(link["envelope"]))
    assert isinstance(prepared, CanonicalPrepared), f"{name} link {index}"
    assert json.loads(prepared.planned_step_json) == link["step"]
    # J1 pass-through: the real kernel reproduces the pinned record byte for byte
    assert prepared.record_bytes.decode() == _canon(link["record"])
    committed = kernel.commit(prepared.prepare_token, prepared.record_digest)
    assert committed.step_seq == index
    assert committed.record_digest == link["record"]["record_digest"]

  assert kernel.lifecycle() == _expected_lifecycle_for(fixture)

  last = links[-1]["step"]["disposition"]
  terminal_json = kernel.terminal_json()
  if last["kind"] == "terminal":
    assert terminal_json is not None, f"{name}: terminal expected"
    assert json.loads(terminal_json) == last["terminal"]
  else:
    assert terminal_json is None


# -- record goldens: normalisation + chain linkage through the real kernel -----


def _assert_record_sans_chain_digests(produced: dict[str, Any], pinned: dict[str, Any]) -> None:
  """The fixture records pin synthetic record.rs helper steps, so step_digest/record_digest
  (and the previous_record_digest chain built on them) are core-unit pins, not kernel-drive
  pins. What a full-directory drive can and must reproduce: the normalised input
  (canonical_input + input_digest) and the envelope identity fields; hash-chain linkage is
  asserted separately against the real digests the kernel produces."""
  strip = lambda record: {
    key: value
    for key, value in record.items()
    if key not in ("record_digest", "step_digest", "previous_record_digest")
  }
  assert strip(produced) == strip(pinned)
  assert produced["record_digest"]
  assert produced["step_digest"]


@pytest.mark.parametrize("name", RECORD_CHAIN)
def test_record_chain_pins_normalisation_and_linkage(name: str) -> None:
  fixture = _read_fixture(name)
  kernel = CanonicalKernel()
  links = fixture["links"]
  assert len(links) > 0

  previous_digest: str | None = None
  for index, link in enumerate(links):
    prepared = kernel.prepare(_canon(link["envelope"]))
    assert isinstance(prepared, CanonicalPrepared), f"{name} link {index}"
    produced = json.loads(prepared.record_bytes.decode())
    _assert_record_sans_chain_digests(produced, link["record"])
    assert produced["previous_record_digest"] == previous_digest
    committed = kernel.commit(prepared.prepare_token, prepared.record_digest)
    assert committed.record_digest == produced["record_digest"]
    previous_digest = committed.record_digest


@pytest.mark.parametrize("name", RECORD_GENESIS)
def test_record_genesis_drives_as_fresh_genesis(name: str) -> None:
  fixture = _read_fixture(name)
  kernel = CanonicalKernel()
  prepared = kernel.prepare(_canon(fixture["envelope"]))
  assert isinstance(prepared, CanonicalPrepared), name
  produced = json.loads(prepared.record_bytes.decode())
  _assert_record_sans_chain_digests(produced, fixture["record"])
  assert produced["previous_record_digest"] is None
  committed = kernel.commit(prepared.prepare_token, prepared.record_digest)
  assert committed.record_digest == produced["record_digest"]


# -- config goldens ------------------------------------------------------------


def _configure_envelope(name: str, config: Any) -> dict[str, Any]:
  return {
    "operation_id": "op-fixture-config",
    "input_id": f"in-{name}",
    "observed_at_ms": "1753747200000",
    "input": {"kind": "configure_operation", "config": config},
  }


@pytest.mark.parametrize("name", CONFIG_RESOLVED)
def test_config_resolved_accepted_at_the_wire(name: str) -> None:
  # the frozen ResolvedOperationConfig comparison is core-side (config.rs); the SDK asserts
  # the fixture config is accepted by a fresh kernel
  fixture = _read_fixture(name)
  kernel = CanonicalKernel()
  prepared = kernel.prepare(_canon(_configure_envelope(name, fixture["config"])))
  assert isinstance(prepared, CanonicalPrepared), name


@pytest.mark.parametrize("name", CONFIG_REJECTS_DEFAULT_LIMITS)
def test_config_reject_fails_with_resolution_stage_fault(name: str) -> None:
  fixture = _read_fixture(name)
  # resolution-stage rejections map to fault codes per binding.rs rejection_fault:
  # policy_violation → invalid_config; collection_too_large currently → malformed_envelope
  # (asymmetry under constitution review — both are §7.7 resolution-stage rejections)
  expected_code = (
    "invalid_config" if fixture["expect"] == "policy_violation" else "malformed_envelope"
  )
  kernel = CanonicalKernel()
  status, code, message = _prepare_fault(kernel, _configure_envelope(name, fixture["config"]))
  assert status == "rejected", name
  assert code == expected_code, f"{name}: {message}"


# -- input goldens: wire acceptance + banned-key scan --------------------------


@pytest.mark.parametrize("name", INPUTS)
def test_input_accepted_at_decode_boundary(name: str) -> None:
  fixture = _read_fixture(name)
  kernel = CanonicalKernel()
  status, code, message = _prepare_fault(kernel, fixture)
  # a decode-stage death is the only failure mode this test tolerates nothing of;
  # lifecycle/authority faults still prove the bytes were wire-valid
  assert not (status == "rejected" and code == "malformed_envelope"), (
    f"{name}: decode-stage rejection: {message}"
  )


def test_no_input_fixture_repeats_envelope_owned_facts() -> None:
  for name in INPUTS:
    keys: set[str] = set()
    _all_keys(_read_fixture(name).get("input", {}), keys)
    for banned in BANNED_INPUT_KEYS:
      assert banned not in keys, f"{name}: business input repeats {banned}"


def test_no_config_fixture_carries_host_owned_facts() -> None:
  for name in [*_by_prefix("input_configure_"), *_by_prefix("golden_config_")]:
    keys: set[str] = set()
    _all_keys(_read_fixture(name), keys)
    for banned in BANNED_CONFIG_KEYS:
      assert banned not in keys, f"{name}: config fixture carries {banned}"


# -- rejection fixtures: fail closed with the declared marker ------------------


@pytest.mark.parametrize("name", ENVELOPE_REJECTS)
def test_envelope_reject_fails_closed(name: str) -> None:
  fixture = _read_fixture(name)
  marker = REJECT_MARKERS.get(fixture["expect"])
  assert marker is not None, f"{name}: no marker registered for expect kind {fixture['expect']}"

  kernel = CanonicalKernel()
  status, code, message = _prepare_fault(kernel, fixture["envelope"])
  assert status == "rejected", name
  assert code == "malformed_envelope", f"{name}: {message}"
  assert marker in message, (
    f"{name}: rejection message must carry the {fixture['expect']} marker, got: {message}"
  )


def test_envelope_reject_fixtures_cover_required_kinds() -> None:
  kinds = {_read_fixture(name)["expect"] for name in ENVELOPE_REJECTS}
  assert "unknown_field" in kinds
  assert "unknown_variant" in kinds
