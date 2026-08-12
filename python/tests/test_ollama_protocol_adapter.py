"""P-04 Ollama ProtocolAdapter lifecycle contracts."""
from __future__ import annotations

import pytest

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.ollama_adapter import OllamaAdapter
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(extensions: dict | None = None):
    return normalize_canonical_adapter_input(
        RenderedContext(turns=[Message(role="user", content="hi")], system_text="system"),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        extensions=extensions,
        resolved=model_registry.resolve_provider_runtime("ollama", "llama3"),
    )


def test_adapter_builds_ollama_request_from_canonical_input() -> None:
    request = OllamaAdapter().build_request(_input({"temperature": 0.2, "model": "wrong", "stream": False}))

    assert request == {
        "temperature": 0.2,
        "model": "llama3",
        "messages": [{"role": "system", "content": "system"}, {"role": "user", "content": "hi"}],
        "tools": [{
            "type": "function",
            "function": {"name": "lookup", "description": "Lookup", "parameters": {"type": "object"}},
        }],
    }


def test_adapter_finalizes_buffered_tool_calls_usage_and_stop_reason() -> None:
    adapter = OllamaAdapter()
    state = adapter.create_stream_state(_input())

    pushed = adapter.push_stream_chunk({
        "message": {
            "content": "working",
            "tool_calls": [{"function": {"name": "lookup", "arguments": {"q": "x"}}}],
        },
    }, state)
    finished = adapter.finish_stream(state, {
        "done": True,
        "done_reason": "length",
        "prompt_eval_count": 12,
        "eval_count": 3,
    })

    assert pushed.events == [TextDelta(delta="working")]
    assert finished.events[0] == ToolCallEvent(id="call_1", name="lookup", arguments={"q": "x"})
    assert finished.events[1] == UsageEvent(
        total_tokens=15,
        input_tokens=12,
        output_tokens=3,
        stop_reason="max_tokens",
        raw_stop_reason="length",
        provider_usage=finished.events[1].provider_usage,
    )


def test_ndjson_decoder_keeps_an_unterminated_final_record_and_skips_malformed_lines() -> None:
    decoder = OllamaAdapter().create_ndjson_decoder()

    assert decoder.push('{"message":{"content":"tail"}}') == []
    assert decoder.finish() == [{"message": {"content": "tail"}}]
    assert decoder.push("not-json\n") == []
    assert decoder.finish() == []


def test_adapter_rejects_malformed_usage_shape() -> None:
    with pytest.raises(ValueError, match="prompt_eval_count"):
        OllamaAdapter().normalize_usage({"prompt_eval_count": "12"})
