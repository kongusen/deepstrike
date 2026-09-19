from types import SimpleNamespace

import pytest

from deepstrike._kernel import ProviderMessage, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.openai_responses import OpenAIResponsesProvider


@pytest.mark.asyncio
async def test_openai_responses_count_tokens_reuses_stateful_request_plan() -> None:
    provider = OpenAIResponsesProvider("test", model="gpt-5.2")
    context = RenderedContext(
        system_text="system",
        turns=[
            ProviderMessage(role="user", content="covered"),
            ProviderMessage(role="assistant", content="covered reply"),
            ProviderMessage(role="user", content="new turn"),
        ],
    )
    tools = [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')]
    captured: dict = {}

    async def count(**params):
        captured.update(params)
        return SimpleNamespace(input_tokens=321)

    provider._client.responses.input_tokens = SimpleNamespace(count=count)
    measurement = await provider.count_tokens(
        context,
        tools,
        extensions={
            "reasoning": {"effort": "medium"},
            "text": {"format": {"type": "text"}},
            "tool_choice": "auto",
            "max_output_tokens": 500,
            "store": False,
        },
        state={"previous_response_id": "resp_1", "covered_message_count": 2},
    )

    assert captured == {
        "model": "gpt-5.2",
        "input": [{"role": "user", "content": "new turn"}],
        "instructions": "system",
        "previous_response_id": "resp_1",
        "tools": [{
            "type": "function",
            "name": "lookup",
            "description": "Lookup",
            "parameters": {"type": "object"},
        }],
        "reasoning": {"effort": "medium"},
        "text": {"format": {"type": "text"}},
        "tool_choice": "auto",
    }
    assert measurement.input_tokens == 321
    assert measurement.source == {"kind": "native", "provider": "openai"}
    assert measurement.confidence == "exact"


@pytest.mark.asyncio
async def test_openai_responses_count_tokens_rejects_custom_endpoint() -> None:
    provider = OpenAIResponsesProvider("test", base_url="https://proxy.invalid/v1")
    with pytest.raises(RuntimeError, match="unavailable"):
        await provider.count_tokens(RenderedContext(system_text="", turns=[]), [])
