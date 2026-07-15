import os
import json
import pytest
from deepstrike.kernel import KernelRuntime, LoopPolicy

def get_fixtures_dir():
    # Start at current file's directory and traverse up
    base = os.path.dirname(__file__)
    for _ in range(4):
        candidate = os.path.join(base, "tests/fixtures/abi")
        if os.path.exists(candidate):
            return candidate
        candidate = os.path.join(base, "../tests/fixtures/abi")
        if os.path.exists(candidate):
            return candidate
        base = os.path.dirname(base)
    raise FileNotFoundError("Could not find tests/fixtures/abi")

def test_golden_start_run():
    fixtures_dir = get_fixtures_dir()
    kernel = KernelRuntime(LoopPolicy())
    
    with open(os.path.join(fixtures_dir, "input_start_run.json"), "r") as f:
        input_json = f.read()
        
    step_json = kernel.step(input_json)
    assert step_json is not None
    
    step = json.loads(step_json)
    assert step["version"] == 2
    assert step.get("faults", []) == []
    assert "actions" in step
    assert len(step["actions"]) > 0
    assert step["actions"][0]["kind"] == "call_provider"

def test_golden_tool_results():
    fixtures_dir = get_fixtures_dir()
    kernel = KernelRuntime(LoopPolicy())

    with open(os.path.join(fixtures_dir, "input_start_run.json"), "r") as f:
        start_json = f.read()
    start_step = json.loads(kernel.step(start_json))
    kernel.step(json.dumps({
        "version": 2,
        "operation_id": "op-golden-001",
        "event_id": "event-provider-001",
        "observed_at_ms": 1710000000500,
        "event": {
            "kind": "provider_result",
            "effect_id": start_step["actions"][0]["effect_id"],
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"id": "call_123", "name": "read", "arguments": {}},
                    {"id": "call_456", "name": "read", "arguments": {}},
                ],
            },
            "now_ms": 1710000000500,
        },
    }))

    with open(os.path.join(fixtures_dir, "input_tool_results.json"), "r") as f:
        input_json = f.read()

    step_json = kernel.step(input_json)
    assert step_json is not None

    step = json.loads(step_json)
    assert step["version"] == 2
    assert step.get("faults", []) == []
    assert "actions" in step

def test_golden_push_artifact():
    fixtures_dir = get_fixtures_dir()
    kernel = KernelRuntime(LoopPolicy())

    with open(os.path.join(fixtures_dir, "input_push_artifact.json"), "r") as f:
        input_json = f.read()

    step_json = kernel.step(input_json)
    assert step_json is not None

    step = json.loads(step_json)
    assert step["version"] == 2
    assert step.get("faults", []) == []
    assert step["actions"] == []
    assert step["observations"] == []
