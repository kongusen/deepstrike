from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, normalize_tool_call

logger = logging.getLogger(__name__)

_DEFAULT_BASE_URL = "http://localhost:11434"


class OllamaProvider:
    def __init__(self, model: str = "llama3", base_url: str = _DEFAULT_BASE_URL, retry_config: RetryConfig | None = None):
        self._model = model
        self._base_url = base_url.rstrip("/")
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool) -> dict:
        body: dict = {
            "model": self._model,
            "messages": [{"role": m.role, "content": m.content} for m in messages],
            "stream": stream,
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
        return body

    async def complete(self, messages: list[Message], tools: list[ToolSchema]) -> Message:
        if self._circuit.is_open():
            raise Exception("Circuit breaker open")

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.post(
                        f"{self._base_url}/api/chat",
                        json=self._build_body(messages, tools, stream=False),
                        timeout=120,
                    )
                    resp.raise_for_status()
                    data = resp.json()

                msg = data.get("message", {})
                content_text = msg.get("content") or ""
                tool_calls = []
                for tc in msg.get("tool_calls") or []:
                    fn = tc.get("function", {})
                    normalized = normalize_tool_call(tc.get("id", ""), fn.get("name", ""), fn.get("arguments", {}))
                    if normalized:
                        tool_calls.append(normalized)

                self._circuit.record_success()
                from deepstrike._kernel import Message as KMessage
                return KMessage(role="assistant", content=content_text, token_count=0, tool_calls=tool_calls)
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    logger.warning("Retry %d/%d after %.1fs: %s", attempt + 1, self._retry.max_retries, delay, exc)
                    await asyncio.sleep(delay)

        raise last_exc or Exception("Complete failed")

    async def stream(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        return self._stream_gen(messages, tools)

    async def _stream_gen(self, messages: list[Message], tools: list[ToolSchema]) -> AsyncIterator[StreamEvent]:
        tool_calls: dict[int, dict] = {}

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/api/chat",
                json=self._build_body(messages, tools, stream=True),
                timeout=120,
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line:
                        continue
                    chunk = json.loads(line)
                    msg = chunk.get("message", {})

                    if text := msg.get("content"):
                        yield TextDelta(delta=text)

                    for idx, tc in enumerate(msg.get("tool_calls") or []):
                        fn = tc.get("function", {})
                        args = fn.get("arguments", {})
                        normalized = normalize_tool_call(tc.get("id", str(idx)), fn.get("name", ""), args)
                        if normalized:
                            yield ToolCallEvent(id=normalized.id, name=normalized.name, arguments=args)
