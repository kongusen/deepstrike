from types import SimpleNamespace

import pytest

from deepstrike._kernel import ModelMessage, ToolSchema
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext


@pytest.mark.asyncio
async def test_anthropic_count_tokens_reuses_generation_request_plan() -> None:
    provider = AnthropicProvider("test", model="claude-sonnet-4-6")
    context = RenderedContext(
        system_text="system",
        turns=[ModelMessage(role="user", content="hello")],
    )
    tools = [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')]
    captured: dict = {}

    async def count_tokens(**params):
        captured.update(params)
        return SimpleNamespace(input_tokens=123)

    provider._client.messages.count_tokens = count_tokens
    measurement = await provider.count_tokens(
        context, tools, extensions={"max_tokens": 321, "cacheBreakpointStrategy": "none"}
    )

    assert captured == {
        "model": "claude-sonnet-4-6",
        "system": "system",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{
            "name": "lookup",
            "description": "Lookup",
            "input_schema": {"type": "object"},
        }],
    }
    assert measurement.input_tokens == 123
    assert measurement.source == {"kind": "native", "provider": "anthropic"}
    assert measurement.confidence == "exact"


@pytest.mark.asyncio
async def test_anthropic_count_tokens_rejects_custom_endpoint() -> None:
    provider = AnthropicProvider("test", base_url="https://proxy.invalid/anthropic")
    with pytest.raises(RuntimeError, match="unavailable"):
        await provider.count_tokens(RenderedContext(system_text="", turns=[]), [])
