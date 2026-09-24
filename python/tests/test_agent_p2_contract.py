import pytest

from deepstrike import create_agent
from deepstrike.memory import InMemoryMemoryStore, MemoryScope
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta


class Provider:
    async def complete(self, context: RenderedContext, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        yield TextDelta(delta="ok")


@pytest.mark.asyncio
async def test_agent_session_remember_and_recall_use_bound_memory():
    store = InMemoryMemoryStore()
    agent = create_agent(
        "memory-agent",
        runtime_binding={
            "provider": Provider(),
            "memory_store": store,
            "memory_scope": MemoryScope("tenant", "default"),
        },
    )
    record = await agent.session("memory-session").remember({"name": "color", "content": "blue"})
    hits = await agent.session("memory-session").recall("blue")
    assert record.content == "blue"
    assert hits and hits[0].record.record_id == record.record_id


@pytest.mark.asyncio
async def test_agent_memory_requires_runtime_binding():
    agent = create_agent("unbound", runtime_binding={"provider": Provider()})
    with pytest.raises(RuntimeError, match="memory is not runtime-bound"):
        await agent.session("s").remember({"name": "x", "content": "y"})


@pytest.mark.asyncio
async def test_memory_only_agent_does_not_require_provider():
    store = InMemoryMemoryStore()
    agent = create_agent(
        "memory-only",
        runtime_binding={"memory_store": store, "memory_scope": MemoryScope("tenant", "default")},
    )
    record = await agent.remember({"name": "fact", "content": "value"})
    assert record.content == "value"


@pytest.mark.asyncio
async def test_agent_delegate_requires_declared_target_and_resolves_host_agent():
    child = create_agent("child", runtime_binding={"provider": Provider()})
    parent = create_agent(
        "parent",
        handoffs=[{"target": "child"}],
        runtime_binding={"provider": Provider(), "agent_resolver": lambda name: child},
    )
    result = await parent.session("s").delegate("child", "say hello")
    assert result["status"] == "completed"
    with pytest.raises(PermissionError):
        await parent.session("s").delegate("other", "blocked")


@pytest.mark.asyncio
async def test_agent_run_returns_structured_execution_evidence():
    agent = create_agent("result", runtime_binding={"provider": Provider()})
    result = await agent.run("hello", session_id="result-session")
    assert result["status"] == "completed"
    assert result["session_id"] == "result-session"
    assert result["run_id"]
