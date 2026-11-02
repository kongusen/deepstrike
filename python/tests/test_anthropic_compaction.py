"""Server-side compaction beyond the plain threshold edit: a threshold compaction that pauses so the
newest history stays verbatim behind the summary, and on-demand compaction that the provider
requests in the background and swaps in for the messages it summarized. Mirrors
node/tests/anthropic-compaction.test.ts. No live API."""
from __future__ import annotations

import copy
from types import SimpleNamespace

import pytest

from deepstrike._kernel import ContentPartObj, ModelMessage, ToolCall
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext


def _text_chunks(text: str, input_tokens: int = 10, output_tokens: int = 5) -> list[dict]:
    return [
        {"type": "message_start", "message": {"id": "msg_text", "usage": {"input_tokens": input_tokens, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": output_tokens}},
    ]


PAUSE_CHUNKS = [
    {"type": "message_start", "message": {"id": "msg_pause", "usage": {"input_tokens": 0, "output_tokens": 0}}},
    {"type": "content_block_start", "index": 0, "content_block": {"type": "compaction", "content": None}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "compaction_delta", "content": "Summary so far."}},
    {"type": "content_block_stop", "index": 0},
    {"type": "message_delta", "delta": {"stop_reason": "compaction"}, "usage": {"output_tokens": 0}},
]

BLOCK = {"type": "compaction", "content": "Summary of u1..a1.", "signature": "sig-1"}


class _Stream:
    def __init__(self, chunks):
        self._chunks = chunks

    async def __aenter__(self):
        async def gen():
            for chunk in self._chunks:
                yield chunk
        return gen()

    async def __aexit__(self, *a):
        return False


class _Messages:
    def __init__(self, fake: "_Client"):
        self._fake = fake

    def stream(self, **kwargs):
        self._fake.streamed.append(copy.deepcopy(kwargs))
        scripts = self._fake.stream_scripts
        return _Stream(scripts.pop(0) if scripts else [])

    async def create(self, **kwargs):
        self._fake.created.append(copy.deepcopy(kwargs))
        if not self._fake.create_responses:
            raise RuntimeError("no scripted response")
        return self._fake.create_responses.pop(0)

    async def count_tokens(self, **kwargs):
        self._fake.counted.append(copy.deepcopy(kwargs))
        return SimpleNamespace(input_tokens=42)


class _Client:
    def __init__(self, streams=None, creates=None):
        self.stream_scripts = list(streams or [])
        self.create_responses = list(creates or [])
        self.streamed: list[dict] = []
        self.created: list[dict] = []
        self.counted: list[dict] = []
        self.messages = _Messages(self)
        self.beta = SimpleNamespace(messages=_Messages(self))


def _install(provider: AnthropicProvider, streams=None, creates=None) -> _Client:
    client = _Client(streams, creates)
    provider._client = client
    return client


def _ctx(turns, state_turn=None) -> RenderedContext:
    return RenderedContext(turns=turns, system_text="stable", system_stable="stable", state_turn=state_turn)


async def _drain(provider: AnthropicProvider, context: RenderedContext, extensions=None) -> str:
    text = ""
    async for event in provider.stream(context, [], extensions):
        delta = getattr(event, "delta", None)
        if type(event).__name__ == "TextDelta":
            text += delta
    return text


def test_compaction_settings_that_do_not_fit_the_mode_are_rejected():
    with pytest.raises(TypeError, match="keep_recent_messages"):
        AnthropicProvider("k", compaction={"pause_after_compaction": {"keep_recent_messages": -1}})
    with pytest.raises(TypeError, match="compact_at_input_tokens"):
        AnthropicProvider("k", compaction={"mode": "on_demand", "compact_at_input_tokens": 0})
    with pytest.raises(TypeError, match="threshold"):
        AnthropicProvider("k", compaction={"mode": "on_demand", "compact_at_input_tokens": 10, "trigger_input_tokens": 60_000})


@pytest.mark.asyncio
async def test_a_paused_turn_continues_with_the_block_appended_then_replays_it_ahead_of_the_reply():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction={
        "trigger_input_tokens": 60_000, "pause_after_compaction": {"keep_recent_messages": 0},
    })
    client = _install(provider, streams=[PAUSE_CHUNKS, _text_chunks("Continued.")])
    first = [ModelMessage(role="user", content="u1")]
    assert await _drain(provider, _ctx(first)) == "Continued."
    assert client.streamed[0]["context_management"]["edits"] == [{
        "type": "compact_20260112", "trigger": {"type": "input_tokens", "value": 60_000}, "pause_after_compaction": True,
    }]
    assert client.streamed[1]["context_management"]["edits"] == [{
        "type": "compact_20260112", "trigger": {"type": "input_tokens", "value": 60_000},
    }]
    assert client.streamed[1]["messages"][-1] == {"role": "assistant", "content": [{"type": "compaction", "content": "Summary so far."}]}

    client = _install(provider, streams=[_text_chunks("ok")])
    nxt = [*first, ModelMessage(role="assistant", content="Continued."), ModelMessage(role="user", content="u2")]
    await _drain(provider, _ctx(nxt))
    # Everything before the block is ignored by the API, so it is not sent.
    assert client.streamed[0]["messages"][0] == {
        "role": "assistant",
        "content": [{"type": "compaction", "content": "Summary so far."}, {"type": "text", "text": "Continued."}],
    }
    assert len(client.streamed[0]["messages"]) == 2


@pytest.mark.asyncio
async def test_the_newest_history_stays_verbatim_behind_the_block_without_its_thinking():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction={"pause_after_compaction": {"keep_recent_messages": 1}})
    call = ToolCall("t1", "look", "{}")
    tool_turn = ModelMessage(role="assistant", content="", tool_calls=[call])
    provider.seed_provider_replay("", [call], {
        "protocol": "anthropic-messages",
        "native_blocks": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "tool_use", "id": "t1", "name": "look", "input": {}},
        ],
    })
    turns = [
        ModelMessage(role="user", content="u1"),
        ModelMessage(role="assistant", content="a1"),
        ModelMessage(role="user", content="u2"),
        tool_turn,
        ModelMessage(role="tool", content="seen", content_parts=[ContentPartObj("tool_result", call_id="t1", output="seen", is_error=False)]),
    ]
    client = _install(provider, streams=[PAUSE_CHUNKS, _text_chunks("Done.")])
    assert await _drain(provider, _ctx(turns)) == "Done."
    anchor = {
        "role": "assistant",
        "content": [
            {"type": "compaction", "content": "Summary so far."},
            {"type": "tool_use", "id": "t1", "name": "look", "input": {}},
        ],
    }
    sent = client.streamed[1]["messages"]
    assert sent[0] == anchor
    assert [m["role"] for m in sent] == ["assistant", "user"]

    client = _install(provider, streams=[_text_chunks("ok")])
    await _drain(provider, _ctx([*turns, ModelMessage(role="assistant", content="Done."), ModelMessage(role="user", content="u3")]))
    assert client.streamed[0]["messages"][0] == anchor
    assert len(client.streamed[0]["messages"]) == 4


@pytest.mark.asyncio
async def test_an_ordinary_response_passes_through_when_the_pause_is_configured():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction={"pause_after_compaction": {"keep_recent_messages": 2}})
    client = _install(provider, streams=[_text_chunks("plain")])
    assert await _drain(provider, _ctx([ModelMessage(role="user", content="u1")])) == "plain"
    assert len(client.streamed) == 1


@pytest.mark.asyncio
async def test_a_paused_non_streaming_turn_continues_the_same_way():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction={"pause_after_compaction": {"keep_recent_messages": 0}})
    client = _install(provider, creates=[
        {"id": "m1", "stop_reason": "compaction", "content": [{"type": "compaction", "content": "S."}], "usage": {"input_tokens": 0, "output_tokens": 0}},
        {"id": "m2", "stop_reason": "end_turn", "content": [{"type": "text", "text": "after"}], "usage": {"input_tokens": 3, "output_tokens": 1}},
    ])
    message = await provider.complete(_ctx([ModelMessage(role="user", content="u1")]), [])
    assert message.content == "after"
    assert client.created[1]["messages"][-1] == {"role": "assistant", "content": [{"type": "compaction", "content": "S."}]}
    assert provider.peek_provider_replay("after", [])["native_blocks"] == [
        {"type": "compaction", "content": "S."},
        {"type": "text", "text": "after"},
    ]


def _on_demand(**extra) -> AnthropicProvider:
    return AnthropicProvider("k", model="claude-opus-5-5", compaction={"mode": "on_demand", "compact_at_input_tokens": 100, **extra})


@pytest.mark.asyncio
async def test_on_demand_declares_compaction_native_and_sends_no_edit():
    provider = _on_demand()
    assert provider.descriptor().native_context == {"compaction": True}
    client = _install(provider, streams=[_text_chunks("hi")])
    await _drain(provider, _ctx([ModelMessage(role="user", content="u1")]))
    assert "context_management" not in client.streamed[0]
    assert "betas" not in client.streamed[0]


@pytest.mark.asyncio
async def test_on_demand_summarizes_the_history_a_turn_sent_then_swaps_the_block_in():
    provider = _on_demand(instructions="Keep the open questions.", max_tokens=4096)
    history = [ModelMessage(role="user", content="u1"), ModelMessage(role="assistant", content="a1"), ModelMessage(role="user", content="u2")]
    state = ModelMessage(role="user", content="[TASK STATE] t")
    client = _install(provider, streams=[_text_chunks("a2", 90, 20)], creates=[
        {"stop_reason": "compaction", "content": [BLOCK], "usage": {}},
    ])
    await _drain(provider, _ctx(history, state))
    await provider.compaction_idle()
    request = client.created[0]
    assert request["compaction"] == {"type": "summarize", "instructions": "Keep the open questions."}
    assert request["betas"] == ["compact-2026-09-04"]
    assert request["max_tokens"] == 4096
    assert "stream" not in request and "context_management" not in request
    # The volatile state turn is not history: the summary covers the history messages only.
    assert [m["role"] for m in request["messages"]] == ["user", "assistant", "user"]
    assert "TASK STATE" not in repr(request["messages"])

    client.stream_scripts.append(_text_chunks("a3"))
    nxt = [*history, ModelMessage(role="assistant", content="a2"), ModelMessage(role="user", content="u3")]
    await _drain(provider, _ctx(nxt, state))
    sent = client.streamed[-1]
    assert sent["betas"] == ["compact-2026-09-04"]
    assert sent["messages"][0] == {"role": "assistant", "content": [BLOCK]}
    assert [m["role"] for m in sent["messages"][1:]] == ["assistant", "user", "user"]


@pytest.mark.asyncio
async def test_on_demand_sends_the_whole_history_when_it_no_longer_matches_the_summary():
    provider = _on_demand()
    history = [ModelMessage(role="user", content="u1")]
    client = _install(provider, streams=[_text_chunks("a1", 200, 1)], creates=[{"stop_reason": "compaction", "content": [BLOCK], "usage": {}}])
    await _drain(provider, _ctx(history))
    await provider.compaction_idle()
    client.stream_scripts.extend([_text_chunks("ok"), _text_chunks("ok")])
    await _drain(provider, _ctx([ModelMessage(role="user", content="renewed goal")]))
    assert "betas" not in client.streamed[-1]
    # The stale adoption is gone for good, even if the old front comes back.
    await _drain(provider, _ctx([*history, ModelMessage(role="assistant", content="a1")]))
    assert "compaction" not in repr(client.streamed[-1]["messages"])


@pytest.mark.asyncio
async def test_on_demand_keeps_the_history_when_no_summary_comes_back_and_asks_again():
    provider = _on_demand()
    client = _install(provider, streams=[_text_chunks("a1", 200, 1), _text_chunks("a2", 200, 1)], creates=[
        {"stop_reason": "max_tokens", "content": [], "usage": {}},
        {"stop_reason": "compaction", "content": [BLOCK], "usage": {}},
    ])
    await _drain(provider, _ctx([ModelMessage(role="user", content="u1")]))
    await provider.compaction_idle()
    turns = [ModelMessage(role="user", content="u1"), ModelMessage(role="assistant", content="a1"), ModelMessage(role="user", content="u2")]
    await _drain(provider, _ctx(turns))
    assert "betas" not in client.streamed[-1]
    await provider.compaction_idle()
    assert len(client.created) == 2


@pytest.mark.asyncio
async def test_on_demand_leaves_out_the_fields_the_api_rejects_on_a_summarization_request():
    provider = _on_demand()
    client = _install(provider, streams=[_text_chunks("a1", 500, 1)], creates=[{"stop_reason": "compaction", "content": [BLOCK], "usage": {}}])
    await _drain(provider, _ctx([ModelMessage(role="user", content="u1")]), {
        "stop_sequences": ["END"],
        "tool_choice": {"type": "any"},
        "output_config": {"effort": "high", "format": {"type": "json_schema"}, "task_budget": {"type": "tokens", "total": 30_000, "remaining": 10_000}},
    })
    await provider.compaction_idle()
    request = client.created[0]
    assert "stop_sequences" not in request and "tool_choice" not in request
    assert request["output_config"] == {"effort": "high", "task_budget": {"type": "tokens", "total": 30_000}}


@pytest.mark.asyncio
async def test_a_requests_cache_markers_never_land_on_the_stored_replay():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction=True)
    native = [{"type": "compaction", "content": "S."}, {"type": "text", "text": "r"}]
    provider.seed_provider_replay("r", [], {"protocol": "anthropic-messages", "native_blocks": copy.deepcopy(native)})
    turns = [ModelMessage(role="user", content="u1"), ModelMessage(role="assistant", content="r")]
    for _ in range(3):
        _install(provider, streams=[_text_chunks("x")])
        await _drain(provider, _ctx(turns))
    assert provider.peek_provider_replay("r", [])["native_blocks"] == native


@pytest.mark.asyncio
async def test_a_history_that_carries_a_compaction_block_is_counted_under_its_beta():
    provider = AnthropicProvider("k", model="claude-opus-5-5", compaction=True)
    provider.seed_provider_replay("r", [], {"protocol": "anthropic-messages", "native_blocks": [
        {"type": "compaction", "content": "S."}, {"type": "text", "text": "r"},
    ]})
    client = _install(provider)
    measurement = await provider.count_tokens(_ctx([
        ModelMessage(role="user", content="u1"), ModelMessage(role="assistant", content="r"), ModelMessage(role="user", content="u2"),
    ]), [])
    assert measurement.input_tokens == 42
    assert client.counted[0]["betas"] == ["compact-2026-01-12"]
    assert client.counted[0]["messages"][0]["content"][0]["type"] == "compaction"
