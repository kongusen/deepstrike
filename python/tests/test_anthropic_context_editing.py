"""Anthropic context editing: the provider sends `clear_tool_uses_20250919` and declares the
tool-result-clearing tier native, so the runner delegates it to the provider. No live API."""
from __future__ import annotations

from types import SimpleNamespace

import pytest

from deepstrike._kernel import ModelMessage
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext
from deepstrike.runtime.runner import delegated_context_tiers, merge_delegated_context_policy

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
    def __init__(self, cap: dict, transport: str):
        self._cap = cap
        self._transport = transport

    def stream(self, **kwargs):
        self._cap["transport"] = self._transport
        self._cap["kwargs"] = kwargs
        return _EmptyStreamCtx()


async def _sent(provider: AnthropicProvider, extensions: dict | None = None) -> dict:
    cap: dict = {}
    provider._client = SimpleNamespace(
        messages=_FakeMessages(cap, "stable"),
        beta=SimpleNamespace(messages=_FakeMessages(cap, "beta")),
    )
    _ = [event async for event in provider.stream(CTX, [], extensions)]
    return cap


@pytest.mark.asyncio
async def test_context_editing_is_off_unless_configured():
    provider = AnthropicProvider("k")
    assert provider.descriptor().native_context == {}
    cap = await _sent(provider)
    assert cap["transport"] == "stable"
    assert "context_management" not in cap["kwargs"]
    assert "betas" not in cap["kwargs"]


@pytest.mark.asyncio
async def test_context_editing_sends_clear_tool_uses_and_declares_the_tier_native():
    provider = AnthropicProvider("k", context_editing={"trigger_input_tokens": 60_000, "keep_tool_uses": 4})
    assert provider.descriptor().native_context == {"tool_result_clearing": True}
    cap = await _sent(provider)
    assert cap["transport"] == "beta"
    assert cap["kwargs"]["betas"] == ["context-management-2025-06-27"]
    assert cap["kwargs"]["context_management"] == {
        "edits": [{
            "type": "clear_tool_uses_20250919",
            "trigger": {"type": "input_tokens", "value": 60_000},
            "keep": {"type": "tool_uses", "value": 4},
        }],
    }


@pytest.mark.asyncio
async def test_context_editing_keeps_caller_betas_and_edits():
    provider = AnthropicProvider("k", context_editing=True)
    cap = await _sent(provider, {
        "betas": ["other-beta"],
        "context_management": {"edits": [{"type": "clear_thinking_20251015"}]},
    })
    assert cap["kwargs"]["betas"] == ["other-beta", "context-management-2025-06-27"]
    assert cap["kwargs"]["context_management"]["edits"] == [
        {"type": "clear_thinking_20251015"},
        {"type": "clear_tool_uses_20250919"},
    ]


def test_native_tiers_lower_onto_delegated_tiers():
    assert delegated_context_tiers(AnthropicProvider("k")) is None
    provider = AnthropicProvider("k", context_editing=True)
    assert delegated_context_tiers(provider) == {"tool_result_clearing": True}
    config: dict = {"context_policy": {"preserve_recent_turns": 3}}
    merge_delegated_context_policy(config, provider)
    assert config["context_policy"] == {"preserve_recent_turns": 3, "delegated_tiers": {"tool_result_clearing": True}}


@pytest.mark.asyncio
async def test_compaction_sends_compact_beside_clear_tool_uses_and_declares_both_tiers():
    provider = AnthropicProvider("k", context_editing=True, compaction={"trigger_input_tokens": 120_000, "instructions": "Keep the open questions."})
    assert provider.descriptor().native_context == {"tool_result_clearing": True, "compaction": True}
    cap = await _sent(provider)
    assert cap["kwargs"]["betas"] == ["context-management-2025-06-27", "compact-2026-01-12"]
    assert cap["kwargs"]["context_management"]["edits"] == [
        {"type": "clear_tool_uses_20250919"},
        {"type": "compact_20260112", "trigger": {"type": "input_tokens", "value": 120_000}, "instructions": "Keep the open questions."},
    ]


class _ChunkStreamCtx:
    def __init__(self, chunks: list[dict]):
        self._chunks = chunks

    async def __aenter__(self):
        async def gen():
            for chunk in self._chunks:
                yield chunk
        return gen()

    async def __aexit__(self, *a):
        return False


@pytest.mark.asyncio
async def test_a_streamed_compaction_block_is_replayed_on_the_next_request():
    provider = AnthropicProvider("k", compaction=True)
    chunks = [
        {"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "compaction", "content": None}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "compaction_delta", "content": "Summary: the user wants a scraper."}},
        {"type": "content_block_stop", "index": 0},
        {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Here it is."}},
        {"type": "content_block_stop", "index": 1},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}},
    ]
    sent: list[dict] = []

    class Messages:
        def stream(self, **kwargs):
            sent.append(kwargs)
            return _ChunkStreamCtx(chunks if len(sent) == 1 else [])

    provider._client = SimpleNamespace(messages=Messages(), beta=SimpleNamespace(messages=Messages()))
    _ = [event async for event in provider.stream(CTX, [])]
    follow_up = RenderedContext(turns=[
        ModelMessage(role="user", content="hi"),
        ModelMessage(role="assistant", content="Here it is."),
        ModelMessage(role="user", content="add retries"),
    ])
    _ = [event async for event in provider.stream(follow_up, [])]
    # The API ignores everything before the block, so the turn that carries it is sent first.
    assert len(sent[1]["messages"]) == 2
    assistant = sent[1]["messages"][0]
    assert assistant["role"] == "assistant"
    assert [block["type"] for block in assistant["content"]] == ["compaction", "text"]
    assert assistant["content"][0]["content"] == "Summary: the user wants a scraper."
