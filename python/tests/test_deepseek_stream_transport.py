"""Phase 1 (transport unification) — characterization lock for DeepSeekProvider.stream after the
raw-httpx → openai-SDK migration. Locks the observable contract (StreamEvent sequence, replay
envelope, request kwargs) and the intended behavior CHANGE: the stream now emits a UsageEvent the
old httpx path silently dropped.

No live API: a fake openai client records the create() kwargs and yields shaped chunks, mirroring
tests/test_provider_streaming_parity.py.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.deepseek import DeepSeekProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message, ToolCall
from deepstrike.providers.stream import TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent


def _delta(*, content=None, reasoning=None, tool_calls=None):
    d = SimpleNamespace(content=content, tool_calls=tool_calls or [])
    if reasoning is not None:
        d.reasoning_content = reasoning
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
async def test_stream_emits_text_toolcall_and_usage():
    """Tool calls buffer across deltas and flush on finish_reason; a UsageEvent is now emitted
    (the old httpx path dropped it) and DeepSeek's prompt_cache_hit_tokens is surfaced."""
    provider = DeepSeekProvider("k", model="deepseek-chat")
    usage = SimpleNamespace(total_tokens=30, prompt_tokens=20, completion_tokens=10, prompt_cache_hit_tokens=8)
    chunks = [
        _chunk(_delta(content="hello ")),
        _chunk(_delta(content="world")),
        _chunk(_delta(tool_calls=[_tc(0, id="call_1", name="look", args='{"q":')])),
        _chunk(_delta(tool_calls=[_tc(0, name="up", args='"x"}')]), finish_reason="tool_calls"),
        _chunk(usage=usage),
    ]
    fake = _wire(provider, chunks)

    events = [e async for e in provider.stream(CTX, [])]

    assert [e.delta for e in events if isinstance(e, TextDelta)] == ["hello ", "world"]
    tool_events = [e for e in events if isinstance(e, ToolCallEvent)]
    assert [(e.name, e.arguments) for e in tool_events] == [("lookup", {"q": "x"})]
    usage_events = [e for e in events if isinstance(e, UsageEvent)]
    assert len(usage_events) == 1
    assert (usage_events[0].total_tokens, usage_events[0].input_tokens, usage_events[0].output_tokens) == (30, 20, 10)
    assert usage_events[0].cache_read_input_tokens == 8
    # DeepSeek must never send prompt_cache_key (it 400s on unknown params).
    assert "prompt_cache_key" not in fake.last_kwargs


@pytest.mark.asyncio
async def test_stream_flushes_tool_calls_without_finish_reason():
    provider = DeepSeekProvider("k", model="deepseek-chat")
    chunks = [
        _chunk(_delta(tool_calls=[_tc(0, id="c1", name="search", args='{"a":1}')])),
        _chunk(_delta(content="done")),  # stream ends with no tool_calls finish_reason
    ]
    _wire(provider, chunks)
    events = [e async for e in provider.stream(CTX, [])]
    tool_events = [e for e in events if isinstance(e, ToolCallEvent)]
    assert [(e.name, e.arguments) for e in tool_events] == [("search", {"a": 1})]


@pytest.mark.asyncio
async def test_stream_reasoning_gated_by_expose_reasoning():
    """reasoning_content surfaces as ThinkingDelta only when expose_reasoning is set, but is always
    captured into the replay envelope."""
    # off: no ThinkingDelta, but envelope still persisted
    provider = DeepSeekProvider("k", model="deepseek-chat")
    _wire(provider, [_chunk(_delta(content="answer", reasoning="step1")), _chunk(_delta(reasoning="step2"))])
    events = [e async for e in provider.stream(CTX, [])]
    assert not any(isinstance(e, ThinkingDelta) for e in events)
    env = provider.peek_provider_replay("answer", [])
    assert env is not None and env["reasoning_content"] == "step1step2"
    assert env["schema_version"] == 2 and env["provider"] == "deepseek"

    # on: ThinkingDelta emitted
    provider2 = DeepSeekProvider("k", model="deepseek-chat")
    _wire(provider2, [_chunk(_delta(content="answer", reasoning="step1"))])
    events2 = [e async for e in provider2.stream(CTX, [], {"expose_reasoning": True})]
    assert [e.delta for e in events2 if isinstance(e, ThinkingDelta)] == ["step1"]


@pytest.mark.asyncio
async def test_stream_reasoner_model_sends_no_tools():
    """deepseek-reasoner does not support tool calling — no `tools` on the wire."""
    from deepstrike._kernel import ToolSchema
    provider = DeepSeekProvider("k", model="deepseek-reasoner")
    fake = _wire(provider, [_chunk(_delta(content="thinking done"))])
    tool = ToolSchema(name="t", description="d", parameters="{}")
    _ = [e async for e in provider.stream(CTX, [tool])]
    # No tool defs reach the wire (the openai SDK drops a None `tools` kwarg).
    assert fake.last_kwargs.get("tools") is None


@pytest.mark.asyncio
async def test_stream_replay_envelope_includes_native_tool_calls():
    provider = DeepSeekProvider("k", model="deepseek-chat")
    _wire(provider, [
        _chunk(_delta(content="ok", reasoning="why"), ),
        _chunk(_delta(tool_calls=[_tc(0, id="c1", name="lookup", args='{}')]), finish_reason="tool_calls"),
    ])
    tool_calls = [ToolCall(id="c1", name="lookup", arguments="{}")]
    _ = [e async for e in provider.stream(CTX, [])]
    env = provider.peek_provider_replay("ok", tool_calls)
    assert env is not None
    assert env["reasoning_content"] == "why"
    assert env["tool_calls"] == [{"id": "c1", "type": "function", "function": {"name": "lookup", "arguments": "{}"}}]


# ── Audit P0: DeepSeek-native thinking control (reasoning_effort + extra_body.thinking) ──
# Vendor-aware: sent only for reasoning models; deepseek-chat passes through clean.

@pytest.mark.asyncio
async def test_reasoning_model_sends_thinking_params_and_strips_control_keys():
    provider = DeepSeekProvider("k", model="deepseek-reasoner")
    fake = _wire(provider, [_chunk(_delta(content="ok"))])
    _ = [e async for e in provider.stream(CTX, [], {"thinking": True, "expose_reasoning": True})]
    assert fake.last_kwargs.get("reasoning_effort") == "high"
    assert fake.last_kwargs.get("extra_body") == {"thinking": {"type": "enabled"}}
    # control keys must not leak as top-level wire params
    for k in ("thinking", "expose_reasoning", "reasoningEffort"):
        assert k not in fake.last_kwargs


@pytest.mark.asyncio
async def test_thinking_false_disables_and_reasoning_effort_max_honored():
    provider = DeepSeekProvider("k", model="deepseek-v4-pro")
    fake = _wire(provider, [_chunk(_delta(content="ok"))])
    _ = [e async for e in provider.stream(CTX, [], {"thinking": False, "reasoning_effort": "max"})]
    assert fake.last_kwargs.get("reasoning_effort") == "max"
    assert fake.last_kwargs.get("extra_body") == {"thinking": {"type": "disabled"}}


@pytest.mark.asyncio
async def test_non_reasoning_model_no_thinking_params():
    # deepseek-chat is non-reasoning: never send reasoning_effort / extra_body (DeepSeek 400s on
    # unknown params), pass extensions through untouched.
    provider = DeepSeekProvider("k", model="deepseek-chat")
    fake = _wire(provider, [_chunk(_delta(content="ok"))])
    _ = [e async for e in provider.stream(CTX, [], {"thinking": True})]
    assert "reasoning_effort" not in fake.last_kwargs
    assert "extra_body" not in fake.last_kwargs
