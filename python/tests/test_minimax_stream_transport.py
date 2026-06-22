"""Characterization lock for MiniMaxOpenAIProvider stream/complete BEFORE the P2 Template-Method
collapse onto the base provider. Locks the observable contract so the collapse is behavior-preserving:
no prompt_cache_key, reasoning_split forced onto the wire, expose_reasoning gating, reasoning_details
capture, schema_v2 replay envelope, UsageEvent.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.minimax import MiniMaxOpenAIProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message, ToolCall
from deepstrike.providers.stream import TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent


def _delta(*, content=None, reasoning=None, details=None, tool_calls=None):
    d = SimpleNamespace(content=content, tool_calls=tool_calls or [])
    if reasoning is not None:
        d.reasoning_content = reasoning
    if details is not None:
        d.reasoning_details = details
    return d


def _tc(index, *, id=None, name=None, args=None):
    return SimpleNamespace(index=index, id=id, function=SimpleNamespace(name=name, arguments=args))


def _chunk(delta=None, finish_reason=None, usage=None):
    if usage is not None:
        return SimpleNamespace(usage=usage, choices=[])
    return SimpleNamespace(usage=None, choices=[SimpleNamespace(delta=delta, finish_reason=finish_reason)])


class _FakeCompletions:
    def __init__(self, chunks):
        self._chunks = chunks
        self.last_kwargs = None

    async def create(self, **kwargs):
        self.last_kwargs = kwargs
        chunks = self._chunks

        async def gen():
            for c in chunks:
                yield c
        return gen()


def _wire(provider, chunks):
    fake = _FakeCompletions(chunks)
    provider._client = SimpleNamespace(chat=SimpleNamespace(completions=fake))
    return fake


CTX = RenderedContext(turns=[Message(role="user", content="hi")])


@pytest.mark.asyncio
async def test_stream_text_usage_and_no_cache_key_but_reasoning_split():
    provider = MiniMaxOpenAIProvider("k")
    usage = SimpleNamespace(total_tokens=12, prompt_tokens=8, completion_tokens=4, prompt_tokens_details=None)
    fake = _wire(provider, [_chunk(_delta(content="hi there")), _chunk(usage=usage)])
    events = [e async for e in provider.stream(CTX, [])]
    assert [e.delta for e in events if isinstance(e, TextDelta)] == ["hi there"]
    usage_events = [e for e in events if isinstance(e, UsageEvent)]
    assert (usage_events[0].total_tokens, usage_events[0].input_tokens, usage_events[0].output_tokens) == (12, 8, 4)
    assert "prompt_cache_key" not in fake.last_kwargs
    # MiniMax forces reasoning_split onto the wire; the internal degrade flag must NOT leak.
    assert fake.last_kwargs.get("reasoning_split") is True
    assert "degrade_missing_reasoning_replay" not in fake.last_kwargs


@pytest.mark.asyncio
async def test_stream_reasoning_split_disabled_passes_through():
    provider = MiniMaxOpenAIProvider("k")
    fake = _wire(provider, [_chunk(_delta(content="x"))])
    _ = [e async for e in provider.stream(CTX, [], {"reasoning_split": False})]
    assert fake.last_kwargs.get("reasoning_split") is False


@pytest.mark.asyncio
async def test_stream_expose_reasoning_gates_thinking_but_details_captured():
    # off: no ThinkingDelta, but reasoning_content + reasoning_details land in the envelope
    provider = MiniMaxOpenAIProvider("k")
    details = [{"type": "reasoning.text", "text": "plan"}]
    _wire(provider, [_chunk(_delta(content="ans", reasoning="plan", details=details))])
    events = [e async for e in provider.stream(CTX, [])]
    assert not any(isinstance(e, ThinkingDelta) for e in events)
    env = provider.peek_provider_replay("ans", [])
    assert env["reasoning_content"] == "plan"
    assert env["reasoning_details"] == details
    assert env["schema_version"] == 2 and env["provider"] == "minimax"

    # on: ThinkingDelta emitted
    provider2 = MiniMaxOpenAIProvider("k")
    _wire(provider2, [_chunk(_delta(content="ans", reasoning="plan"))])
    events2 = [e async for e in provider2.stream(CTX, [], {"expose_reasoning": True})]
    assert [e.delta for e in events2 if isinstance(e, ThinkingDelta)] == ["plan"]


@pytest.mark.asyncio
async def test_stream_tool_calls_and_envelope_native_blocks():
    provider = MiniMaxOpenAIProvider("k")
    _wire(provider, [
        _chunk(_delta(content="ok", reasoning="why")),
        _chunk(_delta(tool_calls=[_tc(0, id="c1", name="look", args='{"q":1}')]), finish_reason="tool_calls"),
    ])
    events = [e async for e in provider.stream(CTX, [])]
    tool_events = [e for e in events if isinstance(e, ToolCallEvent)]
    assert [(e.name, e.arguments) for e in tool_events] == [("look", {"q": 1})]
    env = provider.peek_provider_replay("ok", [ToolCall(id="c1", name="look", arguments='{"q":1}')])
    assert env["reasoning_content"] == "why"
    assert env["tool_calls"] == [{"id": "c1", "type": "function", "function": {"name": "look", "arguments": '{"q":1}'}}]
