"""
01 — OpenAIProvider streaming + CircuitBreaker + normalize_tool_call
"""
import asyncio
import time
import json
import pytest

from deepstrike.providers.base import CircuitBreaker, RetryConfig, normalize_tool_call
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike._kernel import Message, ToolSchema

from conftest import make_provider


# ─── CircuitBreaker (offline) ───────────────────────────────────────────────

class TestCircuitBreaker:
    def test_starts_closed(self):
        cb = CircuitBreaker(RetryConfig())
        assert cb.is_open() is False

    def test_opens_after_threshold(self):
        cb = CircuitBreaker(RetryConfig(circuit_open_after=3))
        cb.record_failure()
        cb.record_failure()
        cb.record_failure()
        assert cb.is_open() is True

    def test_resets_on_success(self):
        cb = CircuitBreaker(RetryConfig(circuit_open_after=2))
        cb.record_failure()
        cb.record_failure()
        cb.record_success()
        assert cb.is_open() is False

    def test_auto_resets_after_timeout(self):
        cb = CircuitBreaker(RetryConfig(circuit_open_after=2, circuit_reset_after=0.05))
        cb.record_failure()
        cb.record_failure()
        assert cb.is_open() is True
        time.sleep(0.06)
        assert cb.is_open() is False


# ─── normalize_tool_call (offline) ──────────────────────────────────────────

class TestNormalizeToolCall:
    def test_empty_name_returns_none(self):
        assert normalize_tool_call("id", "", {}) is None

    def test_json_string_arguments(self):
        tc = normalize_tool_call("id", "tool", '{"x": 1}')
        assert tc is not None
        assert json.loads(tc.arguments) == {"x": 1}

    def test_dict_arguments(self):
        tc = normalize_tool_call("id", "tool", {"y": 2})
        assert tc is not None
        assert json.loads(tc.arguments) == {"y": 2}


# ─── Provider streaming (real API) ─────────────────────────────────────────

class TestOpenAIProvider:
    @pytest.mark.timeout(60)
    async def test_stream_emits_text_delta(self):
        provider = make_provider()
        events = []
        gen = await provider.stream(
            [Message(role="user", content="Reply with exactly: hello")],
            [],
        )
        async for evt in gen:
            events.append(evt)

        full = "".join(e.delta for e in events if isinstance(e, TextDelta))
        assert len(full) > 0
        assert "hello" in full.lower(), f"got: {full}"

    @pytest.mark.timeout(60)
    async def test_stream_produces_tool_call(self):
        provider = make_provider()
        tools = [ToolSchema(
            name="get_time",
            description="Get the current time",
            parameters=json.dumps({"type": "object", "properties": {}, "required": []}),
        )]
        events = []
        gen = await provider.stream(
            [Message(role="user", content="Call get_time right now.")],
            tools,
        )
        async for evt in gen:
            events.append(evt)

        assert any(isinstance(e, ToolCallEvent) for e in events), "expected tool_call event"
