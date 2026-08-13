import json
import os
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
from uuid import uuid4

import pytest


ROOT = Path(__file__).parents[2]
FIXTURES = ROOT / "tests" / "fixtures" / "sdk-conformance" / "canonical"
ADAPTER = ROOT / "scripts" / "sdk-conformance" / "python-adapter.py"


def run_adapter_fixture(fixture: dict) -> dict:
  with TemporaryDirectory(prefix="deepstrike-sdk-conformance-") as directory:
    path = Path(directory) / "fixture.json"
    path.write_text(json.dumps(fixture), encoding="utf-8")
    completed = subprocess.run(
      [sys.executable, str(ADAPTER), str(path)],
      check=True,
      text=True,
      capture_output=True,
    )
    lines = completed.stdout.strip().splitlines()
    assert len(lines) == 1
    return json.loads(lines[0])


def prompt_measurement_fixture(expected_canonical: dict) -> dict:
  return {
    "id": "adapter-focused",
    "domain": "prompt_measurement",
    "input": {
      "value": {
        "requestFingerprint": "sha256:focused",
        "inputTokens": 12,
        "source": {"kind": "heuristic"},
        "confidence": "low_confidence",
      },
    },
    "expected": {"canonical": expected_canonical},
  }


def agent_ir_fixture(reference: str) -> dict:
  return {
    "id": "adapter-focused",
    "domain": "agent_ir",
    "input": {"fixture": reference},
    "expected": {"canonical": {}},
  }


@pytest.mark.parametrize("fixture_path", sorted(FIXTURES.glob("*.json")), ids=lambda path: path.stem)
def test_python_adapter_matches_shared_conformance_fixture(fixture_path: Path) -> None:
  fixture = json.loads(fixture_path.read_text(encoding="utf-8"))
  completed = subprocess.run(
    [sys.executable, str(ADAPTER), str(fixture_path)],
    check=True,
    text=True,
    capture_output=True,
  )
  lines = completed.stdout.strip().splitlines()
  assert len(lines) == 1
  envelope = json.loads(lines[0])

  assert envelope["sdk"] == "python"
  assert envelope["fixture"] == fixture["id"]
  if "canonical" in fixture["expected"]:
    assert envelope == {
      "ok": True,
      "sdk": "python",
      "fixture": fixture["id"],
      "canonical": fixture["expected"]["canonical"],
    }
  else:
    assert envelope["ok"] is False
    assert envelope["error"]["code"] == fixture["expected"]["error"]["code"]
    assert envelope["error"]["path"] == fixture["expected"]["error"]["path"]
    assert isinstance(envelope["error"]["message"], str)


def test_python_adapter_derives_canonical_output_from_sdk_not_expected_shape() -> None:
  envelope = run_adapter_fixture(prompt_measurement_fixture({
    "requestFingerprint": "sha256:focused",
    "inputTokens": 12,
    "source": {"kind": "heuristic"},
    "confidence": "low_confidence",
    "unexpectedList": ["must-not-be-copied"],
  }))

  assert envelope == {
    "ok": True,
    "sdk": "python",
    "fixture": "adapter-focused",
    "canonical": {
      "requestFingerprint": "sha256:focused",
      "inputTokens": 12,
      "source": {"kind": "heuristic"},
      "confidence": "low_confidence",
    },
  }


@pytest.mark.parametrize("reference", [
  str(ROOT / "tests" / "fixtures" / "agent-ir" / "canonical-agent.json"),
  r"\\server\share\agent.json",
  ".",
  "agent-ir/../agent-ir/canonical-agent.json",
], ids=["absolute", "unc", "fixtures-root", "parent-traversal"])
def test_python_adapter_rejects_out_of_boundary_fixture_reference(reference: str) -> None:
  envelope = run_adapter_fixture(agent_ir_fixture(reference))

  assert envelope["ok"] is False
  assert envelope["sdk"] == "python"
  assert envelope["fixture"] == "adapter-focused"
  assert envelope["error"]["code"] == "invalid_fixture_reference"
  assert envelope["error"]["path"] == "/input/fixture"


def test_python_adapter_rejects_fixture_symlink_that_escapes_tests_fixtures(tmp_path: Path) -> None:
  outside = tmp_path / "agent.json"
  outside.write_text((ROOT / "tests" / "fixtures" / "agent-ir" / "canonical-agent.json").read_text(encoding="utf-8"), encoding="utf-8")
  link = ROOT / "tests" / "fixtures" / f".sdk-conformance-escape-{os.getpid()}-{uuid4().hex}.json"
  link.symlink_to(outside)
  try:
    envelope = run_adapter_fixture(agent_ir_fixture(link.name))
    assert envelope["ok"] is False
    assert envelope["sdk"] == "python"
    assert envelope["fixture"] == "adapter-focused"
    assert envelope["error"]["code"] == "invalid_fixture_reference"
    assert envelope["error"]["path"] == "/input/fixture"
  finally:
    link.unlink(missing_ok=True)
