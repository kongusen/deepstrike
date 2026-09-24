from __future__ import annotations

import json

from deepstrike import create_agent, tool


@tool
def add(a: int, b: int) -> int:
    """Add two numbers."""
    return a + b


def test_agent_declaration_is_kernel_safe_and_keeps_host_handlers_private():
    agent = create_agent(
        "math",
        instructions="Use tools carefully.",
        model="openai:gpt",
        tools=[add],
        capability_filter={"allowed_ids": ["add"]},
        metadata={"team": "core"},
    )

    snapshot = agent.declaration
    encoded = json.dumps(snapshot, sort_keys=True)

    assert "add" in encoded
    assert "Use tools carefully." in encoded
    assert all(not callable(value) for value in snapshot.values())
    assert agent._captured.host_tools == (add,)


def test_declaration_lowers_to_shared_run_spec():
    agent = create_agent(
        "math",
        model="openai:gpt",
        tools=[add],
        capability_filter={"allowed_ids": ["add"]},
    )

    spec = agent._captured.declaration.to_run_spec(goal="calculate", session_id="session-1")

    assert spec.identity.agent_id == "math"
    assert spec.identity.session_id == "session-1"
    assert spec.goal == "calculate"
    assert spec.model_hint == "openai:gpt"
    assert spec.capability_filter.allowed_ids == ["add"]
    assert spec.exposure_baseline == ["add"]
