import pytest

from deepstrike._kernel import ContentPartObj, Message, ToolCall
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.deepseek import DeepSeekProvider
from deepstrike.providers.replay_validator import ProviderReplayValidationError
from deepstrike.runtime.provider_replay import (
    is_replay_compatible_with_provider,
    seed_provider_replay_from_events,
)


def _llm_completed(content, tool_calls, provider_replay=None):
    event = {"kind": "llm_completed", "turn": 0, "content": content, "tool_calls": tool_calls}
    if provider_replay is not None:
        event["provider_replay"] = provider_replay
    return event


def test_is_replay_compatible_with_provider_gates_by_protocol():
    anthropic = AnthropicProvider("k")
    deepseek = DeepSeekProvider("k", "deepseek-v4-flash")
    assert is_replay_compatible_with_provider({"protocol": "anthropic-messages"}, anthropic.descriptor()) is True
    assert is_replay_compatible_with_provider({"protocol": "openai-chat"}, anthropic.descriptor()) is False
    # legacy shape inference
    assert is_replay_compatible_with_provider({"native_blocks": [{"type": "text", "text": "x"}]}, deepseek.descriptor()) is False
    assert is_replay_compatible_with_provider({"reasoning_content": "t"}, deepseek.descriptor()) is True
    assert is_replay_compatible_with_provider({"reasoning_content": "t"}, anthropic.descriptor()) is False
    # unknown shape / no descriptor passes through
    assert is_replay_compatible_with_provider({}, anthropic.descriptor()) is True
    assert is_replay_compatible_with_provider({"reasoning_content": "t"}, None) is True


def test_cross_protocol_replay_not_seeded_into_anthropic():
    anthropic = AnthropicProvider("k")
    tool_calls = [ToolCall(id="c1", name="ping", arguments="{}")]
    seed_provider_replay_from_events(anthropic, [_llm_completed(
        "calling", tool_calls,
        provider_replay={"schema_version": 2, "provider": "deepseek", "protocol": "openai-chat", "reasoning_content": "x"},
    )])
    # incompatible envelope skipped entirely; no native blocks seeded
    assert anthropic.peek_provider_replay("calling", tool_calls) is None


def test_legacy_anthropic_log_reconstructs_native_blocks():
    anthropic = AnthropicProvider("k")
    tool_calls = [ToolCall(id="c1", name="ping", arguments='{"a":1}')]
    seed_provider_replay_from_events(anthropic, [_llm_completed("calling", tool_calls)])
    assert anthropic.peek_provider_replay("calling", tool_calls) == {
        "native_blocks": [
            {"type": "text", "text": "calling"},
            {"type": "tool_use", "id": "c1", "name": "ping", "input": {"a": 1}},
        ],
    }


def test_validator_rejects_orphan_tool_result():
    provider = DeepSeekProvider("k", "deepseek-chat")
    context = RenderedContext(turns=[
        Message(role="user", content="hi"),
        Message(role="tool", content="", content_parts=[
            ContentPartObj("tool_result", call_id="orphan", output="x", is_error=False),
        ]),
    ])
    with pytest.raises(ProviderReplayValidationError, match="orphan tool result orphan"):
        provider._build_messages(context)


def test_validator_accepts_matched_tool_result():
    provider = DeepSeekProvider("k", "deepseek-chat")
    context = RenderedContext(turns=[
        Message(role="assistant", content="", tool_calls=[ToolCall(id="c1", name="ping", arguments="{}")]),
        Message(role="tool", content="", content_parts=[
            ContentPartObj("tool_result", call_id="c1", output="pong", is_error=False),
        ]),
    ])
    msgs = provider._build_messages(context)
    assert msgs[-1]["tool_call_id"] == "c1"


def test_deepseek_reasoning_model_fails_fast_without_reasoning_replay():
    provider = DeepSeekProvider("k", "deepseek-v4-flash")
    context = RenderedContext(turns=[
        Message(role="assistant", content="calling", tool_calls=[ToolCall(id="c1", name="ping", arguments="{}")]),
    ])
    with pytest.raises(ProviderReplayValidationError, match="non-empty reasoning_content"):
        provider._build_messages(context)
