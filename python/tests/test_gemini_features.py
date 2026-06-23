"""Gemini vendor features routed through the google-genai `config` dict (verified shapes): thinking,
Google Search grounding, structured output, explicit context-cache reference + create. No live API.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.gemini import GeminiProvider
from deepstrike._kernel import ToolSchema


def _tool(name="f"):
    return ToolSchema(name=name, description="d", parameters="{}")


def test_thinking_config_routed():
    cfg = GeminiProvider("k")._build_config(None, [], {"thinking_config": {"thinking_budget": -1, "include_thoughts": True}})
    assert cfg["thinking_config"] == {"thinking_budget": -1, "include_thoughts": True}


def test_google_search_appends_server_tool_alongside_function_tools():
    cfg = GeminiProvider("k")._build_config("sys", [_tool("lookup")], {"google_search": True})
    tools = cfg["tools"]
    assert tools[0]["function_declarations"][0]["name"] == "lookup"
    assert tools[-1] == {"google_search": {}}
    assert cfg["system_instruction"] == "sys"


def test_google_search_only():
    cfg = GeminiProvider("k")._build_config(None, [], {"google_search": True})
    assert cfg["tools"] == [{"google_search": {}}]


def test_structured_output_keys_routed():
    schema = {"type": "object", "properties": {"x": {"type": "string"}}}
    cfg = GeminiProvider("k")._build_config(None, [], {"response_mime_type": "application/json", "response_schema": schema})
    assert cfg["response_mime_type"] == "application/json"
    assert cfg["response_schema"] == schema


def test_cached_content_reference_routed():
    cfg = GeminiProvider("k")._build_config(None, [], {"cached_content": "cachedContents/abc"})
    assert cfg["cached_content"] == "cachedContents/abc"


def test_no_extensions_keeps_prior_config_shape():
    assert GeminiProvider("k")._build_config(None, []) is None
    cfg = GeminiProvider("k")._build_config("sys", [_tool()])
    assert cfg["system_instruction"] == "sys" and cfg["automatic_function_calling"] == {"disable": True}
    assert "thinking_config" not in cfg and "google_search" not in str(cfg.get("tools"))


@pytest.mark.asyncio
async def test_create_context_cache_calls_caches_create():
    class _FakeCaches:
        def __init__(self):
            self.last = None

        async def create(self, model=None, config=None):
            self.last = {"model": model, "config": config}
            return SimpleNamespace(name="cachedContents/xyz")

    caches = _FakeCaches()
    p = GeminiProvider("k", model="gemini-2.5-flash")
    p._client = SimpleNamespace(aio=SimpleNamespace(caches=caches))  # _require_client returns it (non-None)
    out = await p.create_context_cache(system_instruction="big static prompt", ttl="600s", display_name="d")
    assert out.name == "cachedContents/xyz"
    assert caches.last["model"] == "gemini-2.5-flash"
    cfg = caches.last["config"]
    assert cfg["system_instruction"] == "big static prompt"
    assert cfg["ttl"] == "600s"
    assert cfg["display_name"] == "d"
