"""P-06 OpenAI Responses ProtocolAdapter lifecycle contracts."""
from __future__ import annotations

from copy import deepcopy

import pytest

from deepstrike._kernel import ContentPartObj, Message, ToolCall, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.openai_responses import OpenAIResponsesAdapter
from deepstrike.providers.protocol_adapter import ProtocolResponseError
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(context: RenderedContext | None = None, extensions: dict | None = None):
    return normalize_canonical_adapter_input(
        context or RenderedContext(turns=[Message(role="user", content="hello")]),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        extensions=extensions,
        resolved=model_registry.resolve_provider_runtime("openai", "gpt-4.1"),
    )


def test_adapter_builds_continuation_tail_and_merges_function_and_builtin_tools() -> None:
    context = RenderedContext(turns=[
        Message(role="user", content="find weather"),
        Message(role="assistant", content="", tool_calls=[
            ToolCall(id="call_1", name="lookup", arguments='{"city":"Shanghai"}'),
        ]),
        Message(role="tool", content="", content_parts=[
            ContentPartObj("tool_result", call_id="call_1", output="sunny", is_error=False),
        ]),
    ], system_text="system rules")

    plan = OpenAIResponsesAdapter().build_request(
        _input(context, {"web_search": {"search_context_size": "low"}, "temperature": 0.2}),
        {"previous_response_id": "resp_1", "covered_message_count": 2},
    )

    assert plan.params["previous_response_id"] == "resp_1"
    assert plan.params["input"] == [{
        "type": "function_call_output", "call_id": "call_1", "output": "sunny",
    }]
    assert plan.params["instructions"] == "system rules"
    assert plan.params["temperature"] == 0.2
    assert plan.params["tools"] == [
        {"type": "function", "name": "lookup", "description": "Lookup", "parameters": {"type": "object"}},
        {"type": "web_search", "search_context_size": "low"},
    ]


def test_adapter_decodes_complete_response_and_validates_usage() -> None:
    adapter = OpenAIResponsesAdapter()
    message = adapter.decode_complete({
        "output": [
            {"type": "message", "content": [{"type": "output_text", "text": "done"}]},
            {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": '{"q":"x"}'},
        ],
        "usage": {"input_tokens": 10, "output_tokens": 4, "total_tokens": 14},
    }, _input())

    assert message.content == "done"
    assert message.token_count == 4
    assert [(call.id, call.name, call.arguments) for call in message.tool_calls] == [
        ("call_1", "lookup", '{"q": "x"}'),
    ]

    with pytest.raises(ProtocolResponseError, match="usage.total_tokens"):
        adapter.decode_complete({"output": [], "usage": {"total_tokens": "14"}}, _input())


def test_adapter_stream_returns_state_patch_without_mutating_input_state_or_singleton() -> None:
    adapter = OpenAIResponsesAdapter()
    canonical = _input()
    original_state = {"previous_response_id": "resp_old", "covered_message_count": 1}
    before = deepcopy(original_state)
    state = adapter.create_stream_state(canonical, original_state)

    text = adapter.push_stream_chunk({"type": "response.output_text.delta", "delta": "working"}, state)
    added = adapter.push_stream_chunk({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": ""},
    }, state)
    args = adapter.push_stream_chunk({
        "type": "response.function_call_arguments.done", "output_index": 0, "arguments": '{"q":"x"}',
    }, state)
    tool = adapter.push_stream_chunk({
        "type": "response.output_item.done",
        "output_index": 0,
        "item": {"type": "function_call", "call_id": "call_1", "name": "lookup", "arguments": '{"q":"x"}'},
    }, state)
    completed = adapter.push_stream_chunk({
        "type": "response.completed",
        "response": {
            "id": "resp_new",
            "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12,
                      "input_tokens_details": {"cached_tokens": 4}},
        },
    }, state)

    assert text.events == [TextDelta(delta="working")]
    assert added.events == [] and args.events == []
    assert tool.events == [ToolCallEvent(id="call_1", name="lookup", arguments={"q": "x"})]
    assert completed.run_state_patch == {"previous_response_id": "resp_new", "covered_message_count": 2}
    assert completed.events == [UsageEvent(
        total_tokens=12,
        input_tokens=10,
        output_tokens=2,
        cache_read_input_tokens=4,
        provider_usage=completed.events[0].provider_usage,
    )]
    assert original_state == before
    assert not hasattr(adapter, "function_calls")

