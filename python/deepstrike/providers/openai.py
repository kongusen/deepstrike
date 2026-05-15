from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from openai import AsyncOpenAI
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, normalize_tool_call, to_openai_content

logger = logging.getLogger(__name__)


class OpenAIProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "gpt-4o",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://api.openai.com/v1",
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._client = AsyncOpenAI(api_key=api_key, base_url=base_url)

    def _build_messages(self, messages: list[Message]) -> list[dict]:
        return [{"role": m.role, "content": to_openai_content(m)} for m in messages]

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        if not tools:
            return None
        return [
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

    async def complete(self, messages: list[Message], tools: list[ToolSchema]) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(messages)
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                resp = await self._client.chat.completions.create(
                    model=self._model,
                    messages=msgs,
                    tools=tool_defs,
                )
                self._circuit.record_success()

                choice = resp.choices[0].message
                content = choice.content or ""
                tool_calls: list[ToolCall] = []

                for tc in choice.tool_calls or []:
                    normalized = normalize_tool_call(tc.id, tc.function.name, tc.function.arguments)
                    if normalized:
                        tool_calls.append(normalized)

                return Message(
                    role="assistant",
                    content=content,
                    token_count=resp.usage.total_tokens if resp.usage else None,
                    tool_calls=tool_calls or None,
                )
            except Exception as exc:
                last_exc = exc
                self._circuit.record_failure()
                if attempt < self._retry.max_retries - 1:
                    import asyncio
                    delay = self._retry.base_delay * (2 ** attempt)
                    await asyncio.sleep(delay)

        raise last_exc or RuntimeError("Complete failed")

    async def stream(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        msgs = self._build_messages(messages)
        tool_defs = self._build_tools(tools)
        tool_call_bufs: dict[int, dict] = {}

        stream = await self._client.chat.completions.create(
            model=self._model,
            messages=msgs,
            tools=tool_defs,
            stream=True,
            stream_options={"include_usage": True},
        )

        async for chunk in stream:
            choice = chunk.choices[0] if chunk.choices else None
            if not choice:
                continue

            delta = choice.delta
            if delta.content:
                yield TextDelta(delta=delta.content)

            for tc in delta.tool_calls or []:
                idx = tc.index
                if idx not in tool_call_bufs:
                    tool_call_bufs[idx] = {"id": tc.id or "", "name": "", "args_buf": ""}
                if tc.function and tc.function.name:
                    tool_call_bufs[idx]["name"] += tc.function.name
                if tc.function and tc.function.arguments:
                    tool_call_bufs[idx]["args_buf"] += tc.function.arguments

            if choice.finish_reason == "tool_calls":
                for tb in tool_call_bufs.values():
                    try:
                        args = json.loads(tb["args_buf"] or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
                    if tc_obj:
                        yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)
                tool_call_bufs.clear()
