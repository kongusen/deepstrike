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
    assert step["version"] == 1
    assert "actions" in step
    assert len(step["actions"]) > 0
    assert step["actions"][0]["kind"] == "call_provider"

def test_golden_tool_results():
    fixtures_dir = get_fixtures_dir()
    kernel = KernelRuntime(LoopPolicy())
    
    with open(os.path.join(fixtures_dir, "input_start_run.json"), "r") as f:
        start_json = f.read()
    kernel.step(start_json)
    
    with open(os.path.join(fixtures_dir, "input_tool_results.json"), "r") as f:
        input_json = f.read()
        
    step_json = kernel.step(input_json)
    assert step_json is not None
    
    step = json.loads(step_json)
    assert step["version"] == 1
    assert "actions" in step
