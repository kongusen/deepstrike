from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, normalize_tool_call

logger = logging.getLogger(__name__)

_DEFAULT_BASE_URL = "http://localhost:11434"


class OllamaProvider:
    def __init__(self, model: str = "llama3", base_url: str = _DEFAULT_BASE_URL, retry_config: RetryConfig | None = None):
        self._model = model
        self._base_url = base_url.rstrip("/")
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)

    def _build_body(self, context: RenderedContext, tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        msgs = []
        if context.system_text:
            msgs.append({"role": "system", "content": context.system_text})
        for m in context.turns:
            entry: dict = {"role": m.role, "content": m.content}
            parts = getattr(m, "content_parts", None)
            if parts:
                images = [p.data for p in parts if p.type == "image" and p.data]
                if images:
                    entry["images"] = images
            msgs.append(entry)
        body: dict = {
            **{k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "tools", "stream"}},
            "model": self._model,
            "messages": msgs,
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

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise Exception("Circuit breaker open")

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                async with httpx.AsyncClient() as client:
                    resp = await client.post(
                        f"{self._base_url}/api/chat",
                        json=self._build_body(context, tools, stream=False, extensions=extensions),
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

    def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        return self._stream_gen(context, tools, extensions)

    async def _stream_gen(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        pending_tool_calls: dict[str, dict] = {}

        async with httpx.AsyncClient() as client:
            async with client.stream(
                "POST",
                f"{self._base_url}/api/chat",
                json=self._build_body(context, tools, stream=True, extensions=extensions),
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

                    for tc in msg.get("tool_calls") or []:
                        fn = tc.get("function", {})
                        args = fn.get("arguments", {})
                        normalized = normalize_tool_call("", fn.get("name", ""), args)
                        if normalized:
                            key = f"{normalized.name}:{normalized.arguments}"
                            if key not in pending_tool_calls:
                                pending_tool_calls[key] = {
                                    "id": f"call_{len(pending_tool_calls) + 1}",
                                    "name": normalized.name,
                                    "arguments": args,
                                }

        for tc in pending_tool_calls.values():
            yield ToolCallEvent(id=tc["id"], name=tc["name"], arguments=tc["arguments"])
