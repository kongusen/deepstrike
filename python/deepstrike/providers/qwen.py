from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from http import HTTPStatus
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call, to_openai_message_params
from .replay import ReasoningReplayMixin

logger = logging.getLogger(__name__)

_QWEN_POLICIES: dict[str, RuntimePolicy] = {
    "qwen3.7-max-preview": RuntimePolicy(max_turns=45),
    "qwen3.7-plus-preview": RuntimePolicy(max_turns=40),
    "qwen3.6-max-preview": RuntimePolicy(max_turns=40),
    "qwen3.6-plus": RuntimePolicy(max_turns=35),
    "qwen3.6-flash": RuntimePolicy(max_turns=20),
    "qwen3.6-35b-a3b": RuntimePolicy(max_turns=25),
    "qwen3.6-27b": RuntimePolicy(max_turns=25),
    "qwen3.5-plus": RuntimePolicy(max_turns=35),
    "qwen3.5-flash": RuntimePolicy(max_turns=20),
    "qwen3.5-397b-a17b": RuntimePolicy(max_turns=35),
    "qwen3.5-122b-a10b": RuntimePolicy(max_turns=25),
    "qwen3.5-35b-a3b": RuntimePolicy(max_turns=20),
    "qwen3.5-27b": RuntimePolicy(max_turns=20),
}


class QwenProvider(ReasoningReplayMixin):
    def __init__(
        self,
        api_key: str,
        model: str = "qwen3.6-plus",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ):
        self._api_key = api_key
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url
        self._init_replay_store()
        import dashscope  # noqa: PLC0415
        dashscope.api_key = api_key
        from dashscope import AioGeneration
        from dashscope.api_entities.dashscope_response import Role
        self._generation = AioGeneration
        self._role = Role

    def runtime_policy(self) -> RuntimePolicy:
        return _QWEN_POLICIES.get(self._model, RuntimePolicy())

    def _build_messages(self, context: RenderedContext) -> list[dict]:
        serialized = self._merge_replay_into_openai_messages(
            to_openai_message_params(context),
            context,
        )
        for msg in serialized:
            if msg["role"] == "system":
                msg["role"] = self._role.SYSTEM
            elif msg["role"] == "assistant":
                msg["role"] = self._role.ASSISTANT
            elif msg["role"] == "user":
                msg["role"] = self._role.USER
            elif msg["role"] == "tool":
                msg["role"] = self._role.TOOL
        return serialized

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

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                ext = extensions or {}
                kwargs = {
                    "model": self._model,
                    "messages": msgs,
                    "result_format": "message",
                    "tools": tool_defs,
                }
                if ext.get("enable_thinking") or ext.get("enableThinking"):
                    kwargs["enable_thinking"] = True
                    if "thinking_budget" in ext or "thinkingBudget" in ext:
                        kwargs["thinking_budget"] = int(ext.get("thinking_budget", ext.get("thinkingBudget")))
                for key, value in ext.items():
                    if key not in {"model", "messages", "tools", "stream", "enable_thinking", "enableThinking", "thinking_budget", "thinkingBudget"}:
                        kwargs[key] = value
                resp = await self._generation.call(**kwargs)
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

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
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
        if ext.get("enable_thinking") or ext.get("enableThinking"):
            kwargs["enable_thinking"] = True
            if "thinking_budget" in ext or "thinkingBudget" in ext:
                kwargs["thinking_budget"] = int(ext.get("thinking_budget", ext.get("thinkingBudget")))

        tool_call_bufs: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
        reasoning_content = ""
        final_text = ""
        final_tool_calls: list[ToolCall] = []

        stream = await self._generation.call(**kwargs)
        async for chunk in stream:
            if chunk.status_code != HTTPStatus.OK:
                continue

            choice = chunk.output.choices[0] if chunk.output.choices else None
            if not choice:
                continue

            delta = choice.message
            if hasattr(delta, "reasoning_content") and delta.reasoning_content:
                reasoning_content += str(delta.reasoning_content)
                yield ThinkingDelta(delta=delta.reasoning_content)
            if delta.content:
                final_text += str(delta.content)
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
                for idx, tb in tool_call_bufs.items():
                    if idx in emitted_tool_call_indexes:
                        continue
                    try:
                        args = json.loads(tb["args_buf"] or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
                    if tc_obj:
                        final_tool_calls.append(tc_obj)
                        emitted_tool_call_indexes.add(idx)
                        yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)

        for idx, tb in tool_call_bufs.items():
            if idx in emitted_tool_call_indexes:
                continue
            try:
                args = json.loads(tb["args_buf"] or "{}")
            except json.JSONDecodeError:
                args = {}
            tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
            if tc_obj:
                final_tool_calls.append(tc_obj)
                yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)

        self.remember_reasoning_for_turn(final_text, final_tool_calls, reasoning_content)
