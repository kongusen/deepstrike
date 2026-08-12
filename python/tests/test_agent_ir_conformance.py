from __future__ import annotations

import json
from pathlib import Path

from deepstrike import Agent, lower_agent, normalize_agent


FIXTURE = json.loads(
    (Path(__file__).parents[2] / "tests/fixtures/agent-ir/v1-agent.json").read_text(encoding="utf-8")
)


def test_agent_ir_lowers_the_shared_v1_fixture_without_dropping_extensions():
    spec = lower_agent(normalize_agent(FIXTURE))

    assert spec["version"] == 1
    assert spec["name"] == "researcher"
    assert spec["description"] == "Finds and verifies source material."
    assert spec["instructions"] == "Cite primary sources and state uncertainty."
    assert spec["model"] == FIXTURE["model"]
    assert spec["outputSchema"] == FIXTURE["outputSchema"]
    assert spec["tools"] == FIXTURE["tools"]
    assert spec["memory"] == {"kind": "durable", "namespace": "project-research"}
    assert spec["mcpServers"] == FIXTURE["mcpServers"]
    assert spec["skills"] == FIXTURE["skills"]
    assert spec["knowledge"] == FIXTURE["knowledge"]
    assert spec["handoffs"] == FIXTURE["handoffs"]
    assert spec["guardrails"] == FIXTURE["guardrails"]
    assert spec["metadata"] == FIXTURE["metadata"]
    assert spec["extensions"] == FIXTURE["providerOptions"]
    assert spec["providerOptions"] == spec["extensions"]
    assert spec["inputs"]["context"]["knowledge"] == spec["knowledge"]
    assert spec["inputs"]["capabilities"]["tools"] == spec["tools"]
    assert spec["inputs"]["capabilities"]["mcpServers"] == spec["mcpServers"]
    assert spec["inputs"]["capabilities"]["skills"] == spec["skills"]
    assert spec["inputs"]["memory"] == spec["memory"]
    assert spec["inputs"]["delegation"]["handoffs"] == spec["handoffs"]
    assert spec["inputs"]["governance"]["guardrails"] == spec["guardrails"]
    assert spec["capabilityFilter"] == FIXTURE["capabilityFilter"]
    assert spec["effectiveCapabilities"] == [
        {"kind": "tool", "id": "web_search", "description": "Search the web for source material."},
        {"kind": "skill", "id": "citations", "description": "Citation policy."},
    ]
    assert spec["inputs"]["capabilities"]["effective"] == spec["effectiveCapabilities"]


def test_lowered_agent_ir_is_independent_and_a_filter_cannot_grant_a_capability():
    agent = normalize_agent(FIXTURE)
    spec = lower_agent(agent)
    agent.provider_options["openai"] = {"reasoningEffort": "low"}
    agent.metadata["team"] = "changed"

    assert spec["extensions"]["openai"] == {"reasoningEffort": "high"}
    assert spec["metadata"] == {"team": "research", "priority": 1}

    filtered = lower_agent(Agent(
        name="declared-only",
        tools=[{"name": "read", "parameters": {"type": "object", "properties": {}}}],
        capability_filter={"allowedKinds": ["tool"], "allowedIds": ["not-declared"]},
    ))
    assert filtered["capabilities"] == [{"kind": "tool", "id": "read", "description": ""}]
    assert filtered["effectiveCapabilities"] == []
