from types import SimpleNamespace

import pytest

from deepstrike._kernel import ProviderMessage, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.gemini import GeminiProvider


@pytest.mark.asyncio
async def test_gemini_count_tokens_reuses_generation_request_plan() -> None:
    provider = GeminiProvider("test", model="gemini-2.5-pro")
    context = RenderedContext(
        system_text="system",
        turns=[ProviderMessage(role="user", content="hello")],
    )
    tools = [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')]
    captured: dict = {}

    class Models:
        async def count_tokens(self, **params):
            captured.update(params)
            return SimpleNamespace(total_tokens=234)

    provider._client = SimpleNamespace(aio=SimpleNamespace(models=Models()))
    measurement = await provider.count_tokens(
        context, tools, extensions={"thinking_config": {"thinking_budget": 128}}
    )

    assert captured == {
        "model": "gemini-2.5-pro",
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}],
        "config": {
            "system_instruction": "system",
            "tools": [{"function_declarations": [{
                "name": "lookup",
                "description": "Lookup",
                "parameters_json_schema": {"type": "object"},
            }]}],
            "generation_config": {"thinking_config": {"thinking_budget": 128}},
        },
    }
    assert measurement.input_tokens == 234
    assert measurement.source == {"kind": "native", "provider": "gemini"}
    assert measurement.confidence == "exact"


@pytest.mark.asyncio
async def test_gemini_count_tokens_rejects_custom_endpoint() -> None:
    provider = GeminiProvider("test", base_url="https://proxy.invalid/gemini")
    with pytest.raises(RuntimeError, match="unavailable"):
        await provider.count_tokens(RenderedContext(system_text="", turns=[]), [])
