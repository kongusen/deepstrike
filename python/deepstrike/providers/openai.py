from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from openai import AsyncOpenAI
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call, to_openai_message_params

logger = logging.getLogger(__name__)

_OPENAI_POLICIES: dict[str, RuntimePolicy] = {
    "gpt-4o":       RuntimePolicy(max_turns=25),
    "gpt-4o-mini":  RuntimePolicy(max_turns=15),
    "gpt-4.1":      RuntimePolicy(max_turns=35),
    "gpt-4.1-mini": RuntimePolicy(max_turns=20),
    "gpt-4.1-nano": RuntimePolicy(max_turns=15),
    "gpt-5":        RuntimePolicy(max_turns=50),
    "gpt-5-mini":   RuntimePolicy(max_turns=25),
    "o1":           RuntimePolicy(max_turns=50),
    "o1-mini":      RuntimePolicy(max_turns=25),
    "o3":           RuntimePolicy(max_turns=50),
    "o3-mini":      RuntimePolicy(max_turns=25),
    "o4-mini":      RuntimePolicy(max_turns=25),
}


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
        self._base_url = base_url.rstrip("/")
        self._client = AsyncOpenAI(api_key=api_key, base_url=base_url)
        self._replay_fields: dict[str, dict] = {}

    def runtime_policy(self) -> RuntimePolicy:
        return _OPENAI_POLICIES.get(self._model, RuntimePolicy())

    def _build_messages(self, context: RenderedContext) -> list[dict]:
        serialized = to_openai_message_params(context)
        cursor = 1 if context.system_text else 0

        for source in context.turns:
            if source.role == "tool":
                cursor += sum(
                    1 for p in (getattr(source, "content_parts", None) or [])
                    if p.type == "tool_result"
                )
                continue
            if source.role == "assistant":
                replay = self._replay_fields.get(self._assistant_replay_key(source))
                if replay:
                    serialized[cursor] = {**serialized[cursor], **replay}
            cursor += 1

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

    def _headers(self) -> dict[str, str]:
        return {
            "authorization": f"Bearer {self._client.api_key}",
            "content-type": "application/json",
        }

    def _build_body(
        self,
        messages: list[dict],
        tools: list[ToolSchema],
        stream: bool,
        extensions: dict | None = None,
    ) -> dict:
        body = {
            **{
                k: v
                for k, v in (extensions or {}).items()
                if k not in {"model", "messages", "tools", "stream", "stream_options"}
            },
            "model": self._model,
            "messages": messages,
            "stream": stream,
        }
        tool_defs = self._build_tools(tools)
        if tool_defs:
            body["tools"] = tool_defs
        return body

    def remember_replay_fields(self, message: Message, fields: dict) -> None:
        self._replay_fields[self._assistant_replay_key(message)] = fields

    def _assistant_replay_key(self, message: Message) -> str:
        tool_calls = [
            {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
            for tc in (message.tool_calls or [])
        ]
        return json.dumps({"content": message.content, "tool_calls": tool_calls}, sort_keys=True)

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "tools", "stream", "stream_options"}}
                resp = await self._client.chat.completions.create(
                    **request_extensions,
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

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        msgs = self._build_messages(context)
        tool_defs = self._build_tools(tools)
        tool_call_bufs: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()

        request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "tools", "stream", "stream_options"}}
        stream = await self._client.chat.completions.create(
            **request_extensions,
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
                for idx, tb in tool_call_bufs.items():
                    if idx in emitted_tool_call_indexes:
                        continue
                    try:
                        args = json.loads(tb["args_buf"] or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
                    if tc_obj:
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
                yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)
