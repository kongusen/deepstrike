"""P-03 Gemini ProtocolAdapter lifecycle contracts."""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.gemini_adapter import GeminiAdapter
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(extensions: dict | None = None):
    runtime = model_registry.resolve_provider_runtime("gemini", "gemini-2.0-flash")
    return normalize_canonical_adapter_input(
        RenderedContext(turns=[Message(role="user", content="hello")], system_text="system"),
        [ToolSchema(name="lookup", description="Lookup", parameters="{}")],
        extensions=extensions,
        resolved=runtime,
    )


def test_adapter_builds_gemini_request_plan_from_canonical_input() -> None:
    plan = GeminiAdapter("gemini-2.0-flash").build_request(_input({
        "google_search": True,
        "response_mime_type": "application/json",
    }))

    assert plan.contents == [{"role": "user", "parts": [{"text": "hello"}]}]
    assert plan.config["system_instruction"] == "system"
    assert plan.config["tools"][0]["function_declarations"][0]["name"] == "lookup"
    assert plan.config["tools"][1] == {"google_search": {}}
    assert plan.config["response_mime_type"] == "application/json"


def test_adapter_decodes_complete_response_without_transport_state() -> None:
    response = SimpleNamespace(
        candidates=[SimpleNamespace(content=SimpleNamespace(parts=[
            SimpleNamespace(text="Checking.", function_call=None),
            SimpleNamespace(text=None, function_call=SimpleNamespace(name="lookup", args={"q": "x"})),
        ]))],
        usage_metadata=SimpleNamespace(total_token_count=12),
    )

    message = GeminiAdapter("gemini-2.0-flash").decode_complete(response, _input())

    assert message.content == "Checking."
    assert message.token_count == 12
    assert [(call.id, call.name, call.arguments) for call in message.tool_calls] == [
        ("lookup", "lookup", '{"q": "x"}'),
    ]


def test_adapter_finalizes_stream_tool_calls_usage_and_stop_reason() -> None:
    adapter = GeminiAdapter("gemini-2.0-flash")
    state = adapter.create_stream_state(_input())
    chunk = SimpleNamespace(candidates=[SimpleNamespace(
        content=SimpleNamespace(parts=[
            SimpleNamespace(text="Checking.", function_call=None),
            SimpleNamespace(text=None, function_call=SimpleNamespace(name="lookup", args={"q": "x"})),
        ]),
        finish_reason=None,
    )])

    pushed = adapter.push_stream_chunk(chunk, state)
    final = SimpleNamespace(
        candidates=[SimpleNamespace(content=SimpleNamespace(parts=[]), finish_reason="MAX_TOKENS")],
        usage_metadata=SimpleNamespace(
            total_token_count=105,
            prompt_token_count=80,
            candidates_token_count=25,
            cached_content_token_count=60,
        ),
    )
    finished = adapter.finish_stream(state, final)

    assert pushed.events == [TextDelta(delta="Checking.")]
    assert finished.events[0] == ToolCallEvent(id="call_1", name="lookup", arguments={"q": "x"})
    assert finished.events[1] == UsageEvent(
        total_tokens=105,
        input_tokens=80,
        output_tokens=25,
        cache_read_input_tokens=60,
        stop_reason="max_tokens",
        raw_stop_reason="MAX_TOKENS",
        provider_usage=finished.events[1].provider_usage,
    )


def test_adapter_rejects_malformed_usage_shape() -> None:
    with pytest.raises(ValueError, match="prompt_token_count"):
        GeminiAdapter("gemini-2.0-flash").normalize_usage(
            SimpleNamespace(prompt_token_count="80", candidates_token_count=25)
        )
