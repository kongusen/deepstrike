"""Plan task 9.0 · DeepSeek tool-call markup written as text on the OpenAI-chat protocol, against
the fixture every SDK shares. Mirrors node/tests/text-tool-calls.test.ts."""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from deepstrike._kernel import ModelMessage, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.openai_chat_adapter import OpenAIChatAdapter
from deepstrike.providers.protocol_adapter import ProtocolResponseError
from deepstrike.providers.runtime_registry import OPENAI_CHAT_DIALECTS
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.types.content import normalize_canonical_adapter_input

FIXTURE = json.loads(
    (Path(__file__).resolve().parents[2] / "tests/fixtures/provider-textual-tool-call/openai-chat.json")
    .read_text(encoding="utf-8")
)
PARSED = [case for case in FIXTURE["cases"] if "error" not in case]
FAILING = [case for case in FIXTURE["cases"] if "error" in case]


def _input(provider: str, model: str):
    return normalize_canonical_adapter_input(
        RenderedContext(system_text="system", turns=[ModelMessage(role="user", content="go")]),
        [ToolSchema(name="read_file", description="Read", parameters='{"type":"object"}')],
        extensions={},
        resolved=model_registry.resolve_provider_runtime(provider, model),
    )


def _stream(provider: str, model: str, chunks: list[str]):
    adapter = OpenAIChatAdapter()
    state = adapter.create_stream_state(_input(provider, model), OPENAI_CHAT_DIALECTS[provider])
    events = []
    for content in chunks:
        events.extend(adapter.push_stream_chunk({"choices": [{"delta": {"content": content}}]}, state).events)
    adapter.push_stream_chunk({"choices": [{"delta": {}, "finish_reason": "stop"}]}, state)
    final = adapter.finish_stream(state)
    events.extend(final.events)
    text = "".join(event.delta for event in events if isinstance(event, TextDelta))
    calls = [event for event in events if isinstance(event, ToolCallEvent)]
    return text, calls, final.replay


def _complete(content: str):
    adapter = OpenAIChatAdapter()
    return adapter.decode_complete(
        {"choices": [{"message": {"role": "assistant", "content": content}}]},
        _input("deepseek", "deepseek-chat"), OPENAI_CHAT_DIALECTS["deepseek"],
    ).message


@pytest.mark.parametrize("case", PARSED, ids=[case["name"] for case in PARSED])
def test_streamed_markup_never_reaches_the_text_and_becomes_tool_calls(case) -> None:
    text, calls, replay = _stream("deepseek", "deepseek-chat", case["chunks"])
    assert text == case["text"]
    assert [{"name": call.name, "arguments": call.arguments} for call in calls] == case["calls"]
    assert all(call.id.startswith("call_text_") for call in calls)
    assert len({call.id for call in calls}) == len(calls)
    if calls:
        assert [call["id"] for call in replay["tool_calls"]] == [call.id for call in calls]


@pytest.mark.parametrize("case", PARSED, ids=[case["name"] for case in PARSED])
def test_complete_markup_gives_the_same_text_and_calls(case) -> None:
    message = _complete("".join(case["chunks"]))
    assert message.content == case["text"]
    calls = [{"name": call.name, "arguments": json.loads(call.arguments) if isinstance(call.arguments, str) else call.arguments}
             for call in (message.tool_calls or [])]
    assert calls == case["calls"]


@pytest.mark.parametrize("case", FAILING, ids=[case["name"] for case in FAILING])
def test_unparsed_markup_fails_as_a_retryable_protocol_error(case) -> None:
    for run in (lambda: _stream("deepseek", "deepseek-chat", case["chunks"]), lambda: _complete("".join(case["chunks"]))):
        with pytest.raises(ProtocolResponseError) as raised:
            run()
        assert raised.value.provider_code == case["error"]
        assert raised.value.retryable is True
        assert "logs/a.log" not in str(raised.value)


def test_other_dialects_pass_the_text_through_unchanged() -> None:
    chunks = FIXTURE["cases"][0]["chunks"]
    text, calls, _ = _stream("openai", "gpt-4.1", chunks)
    assert text == "".join(chunks)
    assert calls == []
