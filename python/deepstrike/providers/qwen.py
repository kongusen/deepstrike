from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from http import HTTPStatus
from dashscope import AioGeneration
from dashscope.api_entities.dashscope_response import Role
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, normalize_tool_call

logger = logging.getLogger(__name__)


class QwenProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "qwen-max",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ):
        self._api_key = api_key
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        import dashscope
        dashscope.api_key = api_key

    def _build_messages(self, context: RenderedContext) -> list[dict]:
        result = []
        if context.system_text:
            result.append({"role": Role.SYSTEM, "content": context.system_text})
        for msg in context.turns:
            role = msg.role
            if role == "assistant":
                role = Role.ASSISTANT
            elif role == "user":
                role = Role.USER
            else:
                continue
            result.append({"role": role, "content": msg.content})
        return result

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

    async def complete(self, context: RenderedContext, tools: list[ToolSchema]) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                resp = await AioGeneration.call(
                    model=self._model,
                    messages=msgs,
                    result_format="message",
                    tools=tool_defs,
                )
                if resp.status_code != HTTPStatus.OK:
                    raise RuntimeError(f"DashScope error: {resp.code} - {resp.message}")

                self._circuit.record_success()
                choice = resp.output.choices[0].message
                content = choice.content or ""
                tool_calls: list[ToolCall] = []

                for tc in choice.tool_calls or []:
                    normalized = normalize_tool_call(tc.function.name, tc.function.name, tc.function.arguments)
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

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        ext = extensions or {}
        kwargs = {
            "model": self._model,
            "messages": msgs,
            "result_format": "message",
            "stream": True,
            "incremental_output": True,
        }
        if tool_defs:
            kwargs["tools"] = tool_defs
        if ext.get("enable_thinking"):
            kwargs["enable_thinking"] = True
            if "thinking_budget" in ext:
                kwargs["thinking_budget"] = int(ext["thinking_budget"])

        tool_call_bufs: dict[int, dict] = {}

        stream = await AioGeneration.call(**kwargs)
        async for chunk in stream:
            if chunk.status_code != HTTPStatus.OK:
                continue

            choice = chunk.output.choices[0] if chunk.output.choices else None
            if not choice:
                continue

            delta = choice.message
            if hasattr(delta, "reasoning_content") and delta.reasoning_content:
                yield ThinkingDelta(delta=delta.reasoning_content)
            if delta.content:
                yield TextDelta(delta=delta.content)

            for tc in delta.tool_calls or []:
                idx = getattr(tc, "index", 0)
                if idx not in tool_call_bufs:
                    tool_call_bufs[idx] = {"id": tc.function.name, "name": "", "args_buf": ""}
                if tc.function.name:
                    tool_call_bufs[idx]["name"] = tc.function.name
                if tc.function.arguments:
                    tool_call_bufs[idx]["args_buf"] = tc.function.arguments

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
