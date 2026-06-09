from deepstrike._kernel import Message, ToolCall
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.minimax import MiniMaxAnthropicProvider, MiniMaxOpenAIProvider


def test_minimax_anthropic_descriptor_and_base_url():
    p = MiniMaxAnthropicProvider("k")
    d = p.descriptor()
    assert d.provider == "minimax"
    assert d.protocol == "anthropic-messages"
    assert "minimaxi.com/anthropic" in str(p._client.base_url)


def test_minimax_openai_descriptor_and_base_url():
    p = MiniMaxOpenAIProvider("k")
    d = p.descriptor()
    assert d.provider == "minimax"
    assert d.protocol == "openai-chat"
    assert "minimaxi.com/v1" in p._base_url


def test_minimax_openai_seed_roundtrip_keeps_reasoning_details():
    p = MiniMaxOpenAIProvider("k")
    envelope = {
        "schema_version": 2,
        "provider": "minimax",
        "protocol": "openai-chat",
        "model": "MiniMax-M2.7",
        "reasoning_content": "real plan",
        "reasoning_details": [{"type": "reasoning.text", "text": "real plan"}],
    }
    tool_calls = [ToolCall(id="c1", name="lookup", arguments="{}")]
    p.seed_provider_replay("checking", tool_calls, envelope)
    assert p.peek_provider_replay("checking", tool_calls) == envelope


def test_minimax_openai_wire_filter_does_not_leak_envelope_fields():
    p = MiniMaxOpenAIProvider("k")
    tool_calls = [ToolCall(id="c1", name="lookup", arguments="{}")]
    p.seed_provider_replay("checking", tool_calls, {
        "schema_version": 2,
        "provider": "minimax",
        "protocol": "openai-chat",
        "reasoning_content": "real plan",
        "reasoning_details": [{"type": "reasoning.text", "text": "real plan"}],
        "native_message": {"content": "checking"},
    })
    context = RenderedContext(turns=[
        Message(role="assistant", content="checking", tool_calls=tool_calls),
    ])
    msgs = p._build_messages(context)
    assert msgs[0]["reasoning_content"] == "real plan"
    assert msgs[0]["reasoning_details"] == [{"type": "reasoning.text", "text": "real plan"}]
    for leaked in ("schema_version", "provider", "protocol", "native_message"):
        assert leaked not in msgs[0]
