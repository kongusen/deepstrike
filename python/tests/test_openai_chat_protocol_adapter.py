"""P-07 OpenAI-chat protocol lifecycle and dialect golden contracts."""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.openai_chat_adapter import OpenAIChatAdapter
from deepstrike.providers.protocol_adapter import ProtocolResponseError
from deepstrike.providers.runtime_registry import OPENAI_CHAT_DIALECTS
from deepstrike.providers.stream import TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(provider: str, model: str, extensions: dict | None = None):
    return normalize_canonical_adapter_input(
        RenderedContext(system_text="system", turns=[Message(role="user", content="hello")]),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        extensions=extensions,
        resolved=model_registry.resolve_provider_runtime(provider, model),
    )


@pytest.mark.parametrize(("provider", "model", "extensions", "expected"), [
    ("openai", "gpt-4.1", {"temperature": 0.2}, {"prompt_cache_key": True, "temperature": 0.2}),
    ("deepseek", "deepseek-reasoner", {"thinking": False, "reasoning_effort": "max"}, {
        "reasoning_effort": "max", "extra_body": {"thinking": {"type": "disabled"}}, "no_tools": True,
    }),
    ("kimi", "kimi-k2.6", {"context_cache_id": "cache_1"}, {"cache": {"role": "cache", "content": "cache_id=cache_1"}}),
    ("qwen", "qwen3.6-plus", {"enableThinking": True}, {"enable_thinking": True, "no_cache_key": True}),
    ("glm", "glm-5.2", {"web_search": {"count": 3}}, {"server_tool": {"type": "web_search", "web_search": {"count": 3}}}),
    ("minimax", "MiniMax-M3", {"reasoning_split": True}, {"reasoning_split": True, "no_cache_key": True}),
])
def test_adapter_builds_vendor_request_goldens(provider, model, extensions, expected) -> None:
    adapter = OpenAIChatAdapter()
    plan = adapter.build_request(_input(provider, model, extensions), OPENAI_CHAT_DIALECTS[provider])

    assert plan.params["model"] == model
    assert plan.params["messages"][-1] == {"role": "user", "content": "hello"}
    if expected.get("prompt_cache_key"):
        assert plan.params["prompt_cache_key"].startswith("ds-")
    if expected.get("no_cache_key"):
        assert "prompt_cache_key" not in plan.params
    if expected.get("no_tools"):
        assert "tools" not in plan.params
    if "reasoning_effort" in expected:
        assert plan.params["reasoning_effort"] == expected["reasoning_effort"]
        assert plan.params["extra_body"] == expected["extra_body"]
    if "cache" in expected:
        assert plan.params["messages"][0] == expected["cache"]
    if "server_tool" in expected:
        assert plan.params["tools"][-1] == expected["server_tool"]
    if expected.get("reasoning_split"):
        assert plan.params["reasoning_split"] is True
    if expected.get("enable_thinking"):
        assert plan.params["enable_thinking"] is True


def test_adapter_decodes_minimax_complete_replay_and_rejects_malformed_usage() -> None:
    adapter = OpenAIChatAdapter()
    decoded = adapter.decode_complete({
        "choices": [{"message": {
            "content": "answer", "reasoning_content": "plan", "reasoning_details": {"trace": "x"},
            "tool_calls": [{"id": "call_1", "function": {"name": "lookup", "arguments": '{"q":"x"}'}}],
        }}],
        "usage": {"prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14},
    }, _input("minimax", "MiniMax-M3"), OPENAI_CHAT_DIALECTS["minimax"])

    assert decoded.message.content == "answer"
    assert decoded.message.token_count == 4
    assert decoded.replay == {
        "schema_version": 2, "provider": "minimax", "protocol": "openai-chat", "model": "MiniMax-M3",
        "reasoning_content": "plan", "reasoning_details": {"trace": "x"},
        "tool_calls": [{"id": "call_1", "function": {"name": "lookup", "arguments": '{"q":"x"}'}}],
    }
    with pytest.raises(ProtocolResponseError, match="usage.total_tokens"):
        adapter.decode_complete(
            {"choices": [], "usage": {"total_tokens": "bad"}},
            _input("openai", "gpt-4.1"),
            OPENAI_CHAT_DIALECTS["openai"],
        )


def test_adapter_assembles_stream_events_usage_and_deepseek_replay() -> None:
    adapter = OpenAIChatAdapter()
    state = adapter.create_stream_state(_input("deepseek", "deepseek-chat", {"expose_reasoning": True}), OPENAI_CHAT_DIALECTS["deepseek"])

    thinking = adapter.push_stream_chunk({"choices": [{"delta": {"reasoning_content": "plan"}, "finish_reason": None}]}, state)
    text = adapter.push_stream_chunk({"choices": [{"delta": {"content": "answer"}, "finish_reason": None}]}, state)
    call = adapter.push_stream_chunk({"choices": [{"delta": {"tool_calls": [
        {"index": 0, "id": "call_1", "function": {"name": "lookup", "arguments": '{"q":"x"}'}},
    ]}, "finish_reason": "tool_calls"}]}, state)
    usage = adapter.push_stream_chunk({"usage": {
        "prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12,
        "prompt_tokens_details": {"cached_tokens": 4},
    }}, state)
    finished = adapter.finish_stream(state)

    assert thinking.events == [ThinkingDelta(delta="plan")]
    assert text.events == [TextDelta(delta="answer")]
    assert call.events == [ToolCallEvent(id="call_1", name="lookup", arguments={"q": "x"})]
    assert call.replay["reasoning_content"] == "plan"
    assert usage.events == []
    assert finished.events == [UsageEvent(
        total_tokens=12, input_tokens=10, output_tokens=2, cache_read_input_tokens=4,
        stop_reason="tool_use", raw_stop_reason="tool_calls", provider_usage=finished.events[0].provider_usage,
    )]
