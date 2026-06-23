"""GLM web_search server tool (Zhipu vendor feature, OpenAI-wire only). Enabled via extensions;
injected into the tools array, executed server-side; the selector never leaks as a wire param.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.glm import GLMProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message, ToolSchema


CTX = RenderedContext(turns=[Message(role="user", content="latest news?")])


def _tool(name="f"):
    return ToolSchema(name=name, description="d", parameters="{}")


class _FakeCompletions:
    def __init__(self):
        self.last_kwargs = None

    async def create(self, **kwargs):
        self.last_kwargs = kwargs

        async def gen():
            yield SimpleNamespace(usage=None, choices=[SimpleNamespace(delta=SimpleNamespace(content="ok", tool_calls=[]), finish_reason="stop")])
        return gen()


def _wire(provider):
    fake = _FakeCompletions()
    provider._client = SimpleNamespace(chat=SimpleNamespace(completions=fake), api_key="k")
    return fake


def test_web_search_true_injects_default_tool():
    defs = GLMProvider("k")._wire_tools([], {"web_search": True})
    assert defs == [{"type": "web_search", "web_search": {}}]


def test_web_search_config_passed_through_alongside_function_tools():
    cfg = {"search_engine": "search_pro", "search_recency_filter": "oneWeek"}
    defs = GLMProvider("k")._wire_tools([_tool("lookup")], {"web_search": cfg})
    assert defs[0]["function"]["name"] == "lookup"
    assert defs[1] == {"type": "web_search", "web_search": cfg}


def test_no_web_search_no_injection():
    assert GLMProvider("k")._wire_tools([], {}) is None
    assert GLMProvider("k")._wire_tools([_tool()], {})[0]["type"] == "function"


def test_web_search_selector_stripped_from_wire():
    prepared = GLMProvider("k")._prepare_extensions({"web_search": True, "temperature": 0.3})
    assert prepared == {"temperature": 0.3}


@pytest.mark.asyncio
async def test_stream_sends_web_search_tool_not_the_selector_param():
    provider = GLMProvider("k")
    fake = _wire(provider)
    _ = [e async for e in provider.stream(CTX, [], {"web_search": {"search_engine": "search_pro"}})]
    tools = fake.last_kwargs.get("tools")
    assert tools == [{"type": "web_search", "web_search": {"search_engine": "search_pro"}}]
    assert "web_search" not in fake.last_kwargs  # selector never echoed as a top-level param
    assert "prompt_cache_key" not in fake.last_kwargs  # GLM omits it
