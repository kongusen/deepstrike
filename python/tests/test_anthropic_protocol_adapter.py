"""P-05 Anthropic Messages ProtocolAdapter lifecycle contracts."""
from __future__ import annotations

from types import SimpleNamespace

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.anthropic_adapter import AnthropicMessagesAdapter
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.stream import ThinkingDelta, ToolCallEvent, UsageEvent
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(extensions: dict | None = None):
    return normalize_canonical_adapter_input(
        RenderedContext(
            system_text="stable\n\nknowledge",
            system_stable="stable",
            system_knowledge="knowledge",
            turns=[Message(role="user", content="hi")],
        ),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        extensions=extensions,
        resolved=model_registry.resolve_provider_runtime("anthropic", "claude-sonnet-4-6"),
    )


def test_adapter_builds_stable_and_beta_request_plans() -> None:
    adapter = AnthropicMessagesAdapter()
    stable = adapter.build_request(_input())
    beta = adapter.build_request(_input({"betas": ["code-execution-2025-08-25"]}))

    assert stable.transport == "stable"
    assert beta.transport == "beta"
    assert beta.params["betas"] == ["code-execution-2025-08-25"]
    assert beta.params["messages"] == stable.params["messages"]
    assert stable.params["system"] == [
        {"type": "text", "text": "stable", "cache_control": {"type": "ephemeral"}},
        {"type": "text", "text": "knowledge", "cache_control": {"type": "ephemeral"}},
    ]


def test_adapter_decodes_complete_response_and_returns_native_replay() -> None:
    message, replay = AnthropicMessagesAdapter().decode_complete(SimpleNamespace(
        content=[
            SimpleNamespace(type="thinking", thinking="plan", signature="sig"),
            SimpleNamespace(type="text", text="done"),
            SimpleNamespace(type="tool_use", id="call_2", name="lookup", input={"q": "y"}),
        ],
        usage=SimpleNamespace(input_tokens=10, output_tokens=4),
    ), _input())

    assert message.content == "done"
    assert message.token_count == 14
    assert [(call.id, call.name, call.arguments) for call in message.tool_calls] == [
        ("call_2", "lookup", '{"q": "y"}'),
    ]
    assert replay == {"native_blocks": [
        {"type": "thinking", "thinking": "plan", "signature": "sig"},
        {"type": "text", "text": "done"},
        {"type": "tool_use", "id": "call_2", "name": "lookup", "input": {"q": "y"}},
    ]}


def test_adapter_assembles_stream_usage_tool_json_and_replay() -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_input(), {"system": True, "tools": False, "messages": True})
    events = []
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="message_start",
        message=SimpleNamespace(usage=SimpleNamespace(
            input_tokens=10,
            output_tokens=1,
            cache_read_input_tokens=5,
            cache_creation_input_tokens=2,
        )),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_start",
        index=0,
        content_block=SimpleNamespace(type="thinking", thinking="", signature=""),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=0,
        delta=SimpleNamespace(type="thinking_delta", thinking="plan"),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_start", index=1,
        content_block=SimpleNamespace(type="tool_use", id="call_2", name="lookup", input={}),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=1,
        delta=SimpleNamespace(type="input_json_delta", partial_json='{"q":"y"}'),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(type="content_block_stop", index=1), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="message_delta",
        delta=SimpleNamespace(stop_reason="tool_use"),
        usage=SimpleNamespace(output_tokens=4),
    ), state).events
    finished = adapter.finish_stream(state)

    assert ThinkingDelta(delta="plan") in events
    assert ToolCallEvent(id="call_2", name="lookup", arguments={"q": "y"}) in events
    assert any(isinstance(event, UsageEvent) and event.total_tokens == 21 and event.stop_reason == "tool_use" for event in events)
    assert finished.replay == {"native_blocks": [
        {"type": "thinking", "thinking": "plan", "signature": ""},
        {"type": "tool_use", "id": "call_2", "name": "lookup", "input": {"q": "y"}},
    ]}
