"""AnthropicProvider 1-hour cache TTL (cacheTtl="1h" → {"type":"ephemeral","ttl":"1h"} on every
breakpoint; default = 5m). Also the FIRST test to drive AnthropicProvider.stream end-to-end — it would
have caught the pre-existing _build_system(context, strategy) signature bug (the method took only
`context`, so every complete()/stream() call raised TypeError). No live API.
"""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import ModelMessage
from deepstrike.providers.anthropic_adapter import ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER

CTX = RenderedContext(turns=[ModelMessage(role="user", content="hi")])


class _EmptyStreamCtx:
    async def __aenter__(self):
        async def gen():
            if False:  # noqa: SIM223 — an async generator that yields nothing
                yield
        return gen()

    async def __aexit__(self, *a):
        return False


class _FakeMessages:
    def __init__(self, cap):
        self._cap = cap

    def stream(self, **kwargs):
        self._cap["kwargs"] = kwargs
        return _EmptyStreamCtx()


class _EventStreamCtx:
    def __init__(self, events):
        self._events = events

    async def __aenter__(self):
        async def gen():
            for event in self._events:
                yield event
        return gen()

    async def __aexit__(self, *a):
        return False


class _EventMessages:
    def __init__(self, events):
        self._events = events

    def stream(self, **_kwargs):
        return _EventStreamCtx(self._events)


def _provider():
    p = AnthropicProvider("k")
    cap: dict = {}
    p._client = SimpleNamespace(messages=_FakeMessages(cap))
    return p, cap


def _last_msg_cache_control(cap):
    return cap["kwargs"]["messages"][0]["content"][-1]["cache_control"]


@pytest.mark.asyncio
async def test_stream_runs_and_defaults_to_5m_ephemeral():
    # Running stream at all proves the _build_system signature bug is fixed.
    p, cap = _provider()
    events = [e async for e in p.stream(CTX, [])]
    assert events == []  # empty fake stream
    assert _last_msg_cache_control(cap) == {"type": "ephemeral"}


@pytest.mark.asyncio
async def test_stream_cache_ttl_1h_marks_breakpoints():
    p, cap = _provider()
    _ = [e async for e in p.stream(CTX, [], {"cacheTtl": "1h"})]
    assert _last_msg_cache_control(cap) == {"type": "ephemeral", "ttl": "1h"}
    # the control selector must not leak onto the wire request
    assert "cacheTtl" not in cap["kwargs"]


@pytest.mark.asyncio
async def test_stream_strips_cache_breakpoint_strategy_from_wire():
    p, cap = _provider()
    _ = [e async for e in p.stream(CTX, [], {"cacheBreakpointStrategy": "tools-only"})]
    assert "cacheBreakpointStrategy" not in cap["kwargs"]


@pytest.mark.asyncio
async def test_stream_forwards_harmless_textual_marker_prefix_flushed_at_eof():
    provider = AnthropicProvider(
        "k",
        base_url="https://gateway.example.test/anthropic",
    )
    partial_marker = ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER[:-1]
    provider._client = SimpleNamespace(messages=_EventMessages([
        SimpleNamespace(
            type="content_block_delta",
            index=0,
            delta=SimpleNamespace(type="text_delta", text=f"plain {partial_marker}"),
        ),
    ]))

    events = [event async for event in provider.stream(
        CTX,
        [SimpleNamespace(name="lookup", description="Lookup", parameters='{"type":"object"}')],
    )]
    assert "".join(event.delta for event in events if event.type == "text_delta") == f"plain {partial_marker}"
