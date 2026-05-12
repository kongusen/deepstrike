from __future__ import annotations
import asyncio
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, DoneEvent
from .base import RetryConfig, CircuitBreaker, normalize_tool_call, TokenUsage, to_anthropic_content

logger = logging.getLogger(__name__)


class AnthropicProvider:
    def __init__(self, api_key: str, model: str = "claude-sonnet-4-6", retry_config: RetryConfig | None = None):
        self._api_key = api_key
        self._model = model
        self._base_url = "https://api.anthropic.com/v1"
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)

    def _headers(self) -> dict:
        return {
            "x-api-key": self._api_key,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        }

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool) -> dict:
        body: dict = {
            "model": self._model,
            "max_tokens": 8096,
            "messages": [
                {"role": m.role, "content": to_anthropic_content(m)}
                for m in messages
                if m.role != "system"
            ],
        }
        system_parts = [m.content for m in messages if m.role == "system"]
        if system_parts:
            body["system"] = "\n\n".join(system_parts)
        if tools:
            body["tools"] = [
                {
                    "name": t.name,
                    "description": t.description,
                    "input_schema": json.loads(t.parameters),
                }
                for t in tools
            ]
        if stream:
            body["stream"] = True
        return body

    async def complete(self, messages: list[Message], tools: list[ToolSchema]) -> Message:
        if self._circuit.is_open():
            raise Exception("Circuit breaker open")

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.post(
                        f"{self._base_url}/messages",
                        headers=self._headers(),
                        json=self._build_body(messages, tools, stream=False),
                        timeout=120,
                    )
                    resp.raise_for_status()
                    data = resp.json()

                content_text = ""
                tool_calls: list[ToolCall] = []
                for block in data.get("content", []):
                    if block["type"] == "text":
                        content_text += block["text"]
                    elif block["type"] == "tool_use":
                        tc = normalize_tool_call(block["id"], block["name"], block.get("input", {}))
                        if tc:
                            tool_calls.append(tc)

                usage = data.get("usage", {})
                input_tok = usage.get("input_tokens", 0)
                output_tok = usage.get("output_tokens", 0)
                self._circuit.record_success()
                return Message(
                    role="assistant",
                    content=content_text,
                    token_count=input_tok + output_tok,
                    tool_calls=tool_calls,
                )
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    delay = self._retry.base_delay * (2 ** attempt)
                    logger.warning("Retry %d/%d after %.1fs: %s", attempt + 1, self._retry.max_retries, delay, exc)
                    await asyncio.sleep(delay)

        raise last_exc or Exception("Complete failed")

    async def stream(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        return self._stream_gen(messages, tools)

    async def _stream_gen(self, messages: list[Message], tools: list[ToolSchema]) -> AsyncIterator[StreamEvent]:
        # per-block accumulator: block_index -> {id, name, args_buf}
        tool_blocks: dict[int, dict] = {}
        final_tool_calls: list[ToolCall] = []
        final_text = ""
        total_tokens = 0

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/messages",
                headers=self._headers(),
                json=self._build_body(messages, tools, stream=True),
                timeout=120,
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line.startswith("data:"):
                        continue
                    raw = line[5:].strip()
                    if raw in ("", "[DONE]"):
                        continue
                    evt = json.loads(raw)
                    etype = evt.get("type")

                    if etype == "content_block_start":
                        idx = evt["index"]
                        block = evt["content_block"]
                        if block["type"] == "tool_use":
                            tool_blocks[idx] = {"id": block["id"], "name": block["name"], "args_buf": ""}

                    elif etype == "content_block_delta":
                        idx = evt["index"]
                        delta = evt["delta"]
                        dtype = delta.get("type")
                        if dtype == "text_delta":
                            final_text += delta["text"]
                            yield TextDelta(delta=delta["text"])
                        elif dtype == "thinking_delta":
                            yield ThinkingDelta(delta=delta["thinking"])
                        elif dtype == "input_json_delta" and idx in tool_blocks:
                            tool_blocks[idx]["args_buf"] += delta.get("partial_json", "")

                    elif etype == "content_block_stop":
                        idx = evt["index"]
                        if idx in tool_blocks:
                            tb = tool_blocks.pop(idx)
                            try:
                                args = json.loads(tb["args_buf"] or "{}")
                            except json.JSONDecodeError:
                                args = {}
                            tc = normalize_tool_call(tb["id"], tb["name"], args)
                            if tc:
                                final_tool_calls.append(tc)
                                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

                    elif etype == "message_delta":
                        usage = evt.get("usage", {})
                        total_tokens += usage.get("output_tokens", 0)

                    elif etype == "message_start":
                        usage = evt.get("message", {}).get("usage", {})
                        total_tokens += usage.get("input_tokens", 0)
