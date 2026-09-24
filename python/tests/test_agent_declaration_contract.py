from __future__ import annotations

import json

import pytest

from deepstrike import InMemoryAgentResolver, create_agent, resolve_handoff, tool


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


def test_agent_resolver_is_name_only_and_returns_captured_declaration():
    agent = create_agent("math", tools=[add])
    captured = agent._captured
    resolver = InMemoryAgentResolver([captured])

    assert resolver.resolve("math") is captured


def test_handoff_requires_explicit_declaration():
    source = create_agent("source", handoffs=[{"target": "math"}])
    target = create_agent("math")
    resolution = resolve_handoff(
        source._captured.declaration,
        "math",
        "finish the calculation",
        InMemoryAgentResolver([target._captured]),
    )
    assert resolution.target is target._captured
    assert resolution.goal == "finish the calculation"

    with pytest.raises(PermissionError):
        resolve_handoff(source._captured.declaration, "other", "goal", InMemoryAgentResolver())
