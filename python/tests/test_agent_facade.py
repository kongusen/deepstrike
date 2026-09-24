from __future__ import annotations

from deepstrike import AgentSession, InMemoryMemoryStore, MemoryScope, RunResult, create_agent
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta
import pytest


class OneTurnProvider:
    async def complete(self, context: RenderedContext, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        yield TextDelta(delta="hello")


async def test_agent_run_returns_attribute_and_mapping_result():
    agent = create_agent("greeter", runtime_binding={"provider": OneTurnProvider()})

    result = await agent.run("say hello")

    assert isinstance(result, RunResult)
    assert result.output == "hello"
    assert result["output"] == "hello"
    assert result.session_id.startswith("agent-")
    assert result.run_id


async def test_agent_memory_apis_use_runtime_binding():
    store = InMemoryMemoryStore()
    agent = create_agent(
        "greeter",
        runtime_binding={
            "provider": OneTurnProvider(),
            "memory_store": store,
            "memory_scope": MemoryScope("tenant", "greeter"),
        },
    )

    record = await agent.remember("remember this", name="note")
    hits = await agent.recall("remember this")

    assert record.content == "remember this"
    assert hits and hits[0].record.record_id == record.record_id


async def test_agent_output_schema_is_reported_on_run_result():
    agent = create_agent(
        "structured",
        output_schema={"type": "object", "required": ["answer"]},
        runtime_binding={"provider": type("Provider", (), {
            "complete": OneTurnProvider.complete,
            "stream": lambda self, context, tools, extensions=None, state=None: _structured_stream(),
        })()},
    )

    result = await agent.run("return json")

    assert result.output_validation is not None
    assert result.output_validation["valid"] is False
    assert result.status == "partial"


async def _structured_stream():
    yield TextDelta(delta="not json")


def test_agent_session_is_pythonic_and_stable():
    agent = create_agent("greeter", runtime_binding={"provider": OneTurnProvider()})

    session = agent.session("session-1")

    assert isinstance(session, AgentSession)
    assert session.id == "session-1"
    assert agent.session("session-1").id == session.id


def test_agent_captures_invalid_bindings_before_runtime_creation():
    with pytest.raises(ValueError, match="configured together"):
        create_agent("invalid", runtime_binding={"memory_store": InMemoryMemoryStore()})

    with pytest.raises(ValueError, match="requires a name"):
        create_agent("invalid", mcp_servers=[{"command": "server"}])
