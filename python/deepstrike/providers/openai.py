from __future__ import annotations
import asyncio
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent, DoneEvent
from .base import RetryConfig, CircuitBreaker, normalize_tool_call

logger = logging.getLogger(__name__)


class OpenAIProvider:
    def __init__(self, api_key: str, model: str = "gpt-4o", retry_config: RetryConfig | None = None, base_url: str = "https://api.openai.com/v1"):
        self._api_key = api_key
        self._model = model
        self._base_url = base_url
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)

    def _headers(self) -> dict:
        return {
            "Authorization": f"Bearer {self._api_key}",
            "Content-Type": "application/json",
        }

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool) -> dict:
        body: dict = {
            "model": self._model,
            "messages": [{"role": m.role, "content": m.content} for m in messages],
        }
        if tools:
            body["tools"] = [
                {
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": json.loads(t.parameters),
                    },
                }
                for t in tools
            ]
        if stream:
            body["stream"] = True
            body["stream_options"] = {"include_usage": True}
        return body

    async def complete(self, messages: list[Message], tools: list[ToolSchema]) -> Message:
        if self._circuit.is_open():
            raise Exception("Circuit breaker open")

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.post(
                        f"{self._base_url}/chat/completions",
                        headers=self._headers(),
                        json=self._build_body(messages, tools, stream=False),
                        timeout=120,
                    )
                    resp.raise_for_status()
                    data = resp.json()

                choice = data["choices"][0]["message"]
                content_text = choice.get("content") or ""
                tool_calls: list[ToolCall] = []
                for tc in choice.get("tool_calls") or []:
                    normalized = normalize_tool_call(tc["id"], tc["function"]["name"], tc["function"].get("arguments", "{}"))
                    if normalized:
                        tool_calls.append(normalized)

                usage = data.get("usage", {})
                self._circuit.record_success()
                return Message(
                    role="assistant",
                    content=content_text,
                    token_count=usage.get("total_tokens", 0),
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
        # per-index accumulator for tool calls
        tool_calls: dict[int, dict] = {}

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/chat/completions",
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
                    chunk = json.loads(raw)
                    choice = (chunk.get("choices") or [{}])[0]
                    delta = choice.get("delta", {})

                    if text := delta.get("content"):
                        yield TextDelta(delta=text)

                    for tc_delta in delta.get("tool_calls") or []:
                        idx = tc_delta["index"]
                        if idx not in tool_calls:
                            tool_calls[idx] = {"id": tc_delta.get("id", ""), "name": "", "args_buf": ""}
                        fn = tc_delta.get("function", {})
                        if fn.get("name"):
                            tool_calls[idx]["name"] += fn["name"]
                        tool_calls[idx]["args_buf"] += fn.get("arguments", "")

                    if choice.get("finish_reason") == "tool_calls":
                        for tb in tool_calls.values():
                            try:
                                args = json.loads(tb["args_buf"] or "{}")
                            except json.JSONDecodeError:
                                args = {}
                            tc = normalize_tool_call(tb["id"], tb["name"], args)
                            if tc:
                                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)
                        tool_calls.clear()
