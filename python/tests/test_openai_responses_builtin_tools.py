"""OpenAI Responses API built-in (server) tools: web_search / file_search / code_interpreter sit in
the same tools[] list as function tools; the selectors are stripped from the wire request.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.openai_responses import OpenAIResponsesProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message, ToolSchema

CTX = RenderedContext(turns=[Message(role="user", content="latest news?")])


def _tool(name="f"):
    return ToolSchema(name=name, description="d", parameters="{}")


def test_web_search_true_and_config():
    p = OpenAIResponsesProvider("k")
    assert p._builtin_tools({"web_search": True}) == [{"type": "web_search"}]
    assert p._builtin_tools({"web_search": {"search_context_size": "high"}}) == [
        {"type": "web_search", "search_context_size": "high"}
    ]


def test_builtin_tools_passthrough_for_file_and_code():
    p = OpenAIResponsesProvider("k")
    extra = [
        {"type": "file_search", "vector_store_ids": ["vs_1"]},
        {"type": "code_interpreter", "container": {"type": "auto"}},
    ]
    assert p._builtin_tools({"builtin_tools": extra}) == extra


def test_all_tools_combines_function_and_builtin():
    p = OpenAIResponsesProvider("k")
    defs = p._all_tools([_tool("lookup")], {"web_search": True})
    assert defs[0]["type"] == "function" and defs[0]["name"] == "lookup"
    assert defs[-1] == {"type": "web_search"}


def test_selectors_stripped_from_wire_extensions():
    p = OpenAIResponsesProvider("k")
    req = p._request_extensions({"web_search": True, "builtin_tools": [{"type": "file_search"}], "temperature": 0.4})
    assert "web_search" not in req and "builtin_tools" not in req
    assert req["temperature"] == 0.4


@pytest.mark.asyncio
async def test_stream_sends_builtin_tool_not_selector():
    p = OpenAIResponsesProvider("k")

    class _FakeResponses:
        def __init__(self):
            self.last = None

        async def create(self, **kwargs):
            self.last = kwargs

            async def gen():
                if False:
                    yield
            return gen()

    fake = _FakeResponses()
    p._client = SimpleNamespace(responses=fake)
    _ = [e async for e in p.stream(CTX, [], {"web_search": {"search_context_size": "low"}})]
    assert fake.last["tools"] == [{"type": "web_search", "search_context_size": "low"}]
    assert "web_search" not in fake.last  # selector not echoed as a top-level param
