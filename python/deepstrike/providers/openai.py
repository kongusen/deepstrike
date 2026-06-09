from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from openai import AsyncOpenAI
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent, ThinkingDelta
from .base import RetryConfig, CircuitBreaker, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, to_openai_message_params, ThinkingTagStreamExtractor
from .replay import ReasoningReplayMixin, assistant_replay_key
from .replay_validator import validate_openai_chat_replay

logger = logging.getLogger(__name__)

_OPENAI_POLICIES: dict[str, RuntimePolicy] = {
    "gpt-5.5":      RuntimePolicy(max_turns=60),
    "gpt-5.4":      RuntimePolicy(max_turns=50),
    "gpt-5.4-mini": RuntimePolicy(max_turns=25),
    "gpt-5.4-nano": RuntimePolicy(max_turns=15),
    "gpt-5.2":      RuntimePolicy(max_turns=50),
    "gpt-5.2-pro":  RuntimePolicy(max_turns=60),
    "gpt-5.1":      RuntimePolicy(max_turns=50),
    "gpt-4o":       RuntimePolicy(max_turns=25),
    "gpt-4o-mini":  RuntimePolicy(max_turns=15),
    "gpt-4.1":      RuntimePolicy(max_turns=35),
    "gpt-4.1-mini": RuntimePolicy(max_turns=20),
    "gpt-4.1-nano": RuntimePolicy(max_turns=15),
    "gpt-5":        RuntimePolicy(max_turns=50),
    "gpt-5-pro":    RuntimePolicy(max_turns=60),
    "gpt-5-mini":   RuntimePolicy(max_turns=25),
    "gpt-5-nano":   RuntimePolicy(max_turns=15),
    "o1":           RuntimePolicy(max_turns=50),
    "o1-mini":      RuntimePolicy(max_turns=25),
    "o3":           RuntimePolicy(max_turns=50),
    "o3-mini":      RuntimePolicy(max_turns=25),
    "o4-mini":      RuntimePolicy(max_turns=25),
}


class OpenAIProvider(ReasoningReplayMixin):
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
        self._init_replay_store()

    def runtime_policy(self) -> RuntimePolicy:
        return _OPENAI_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="openai",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": False},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        return False

    def _build_messages(self, context: RenderedContext, extensions: dict | None = None) -> list[dict]:
        validate_openai_chat_replay(
            context.turns,
            descriptor=self.descriptor(),
            require_non_empty_reasoning_for_tool_calls=self._require_non_empty_reasoning_replay_for_tool_turns(extensions),
            replay_for_assistant=lambda content, tool_calls: self._replay_fields.get(
                assistant_replay_key(content, tool_calls)
            ),
        )
        serialized = to_openai_message_params(context)
        return self._merge_replay_into_openai_messages(serialized, context)

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

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context, extensions)
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
        msgs = self._build_messages(context, extensions)
        tool_defs = self._build_tools(tools)
        tool_call_bufs: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
        extractor = ThinkingTagStreamExtractor()
        accumulated_reasoning = ""
        accumulated_content = ""
        final_tool_calls = []

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

            delta = getattr(choice, "delta", None)
            if not delta:
                continue

            native_reasoning = getattr(delta, "reasoning_content", None)
            if native_reasoning:
                accumulated_reasoning += native_reasoning
                yield ThinkingDelta(delta=native_reasoning)

            if delta.content:
                for part in extractor.feed(delta.content):
                    if part["type"] == "thinking":
                        accumulated_reasoning += part["content"]
                        yield ThinkingDelta(delta=part["content"])
                    else:
                        accumulated_content += part["content"]
                        yield TextDelta(delta=part["content"])

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
                        final_tool_calls.append(tc_obj)
                        emitted_tool_call_indexes.add(idx)
                        yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)
                self.remember_reasoning_for_turn(accumulated_content, final_tool_calls, accumulated_reasoning)

        for part in extractor.flush():
            if part["type"] == "thinking":
                accumulated_reasoning += part["content"]
                yield ThinkingDelta(delta=part["content"])
            else:
                accumulated_content += part["content"]
                yield TextDelta(delta=part["content"])

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

        self.remember_reasoning_for_turn(accumulated_content, final_tool_calls, accumulated_reasoning)
