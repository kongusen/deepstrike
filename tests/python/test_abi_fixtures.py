"""Golden ABI fixture tests — Python host binding."""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from deepstrike._kernel import KernelRuntime, LoopPolicy

FIXTURES = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "abi"


def _load(name: str) -> dict:
    return json.loads((FIXTURES / name).read_text())


@pytest.mark.timeout(30)
def test_input_spawn_sub_agent_emits_agent_process_changed() -> None:
    runtime = KernelRuntime(LoopPolicy(max_tokens=2048))
    runtime.step(json.dumps(_load("input_start_run.json")))

    step = json.loads(runtime.step(json.dumps(_load("input_spawn_sub_agent.json"))))
    assert step["version"] == 1
    assert step["actions"] == []
    proc = next(o for o in step["observations"] if o.get("kind") == "agent_process_changed")
    assert proc["agent_id"] == "worker"
    assert proc["parent_session_id"] == "parent-session-001"
    assert proc["state"] == "running"
    assert any(o.get("kind") == "suspended" and o.get("reason") == "sub_agent_await" for o in step["observations"])


def test_observation_agent_process_changed_fixture_fields() -> None:
    obs = _load("observation_agent_process_changed.json")
    assert obs["kind"] == "agent_process_changed"
    assert "read_file" in obs["permitted_capability_ids"]


@pytest.mark.parametrize("filename,expected", [
    ("observation_checkpoint_taken.json",   {"kind": "checkpoint_taken",   "turn": 2, "history_len": 4}),
    ("observation_renewed.json",            {"kind": "renewed",            "sprint": 2}),
    ("observation_rollbacked.json",         {"kind": "rollbacked",         "turn": 2, "checkpoint_history_len": 3}),
    ("observation_capability_changed.json", {"kind": "capability_changed", "turn": 1, "capability_id": "write_file"}),
    ("observation_milestone_advanced.json", {"kind": "milestone_advanced", "turn": 3, "phase_id": "phase-1"}),
    ("observation_milestone_blocked.json",  {"kind": "milestone_blocked",  "turn": 3, "phase_id": "phase-1"}),
])
def test_observation_fixture_fields(filename: str, expected: dict) -> None:
    obs = _load(filename)
    for k, v in expected.items():
        assert obs[k] == v, f"{filename}: expected {k}={v!r}, got {obs[k]!r}"
