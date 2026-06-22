from __future__ import annotations

import json
from typing import AsyncIterator

from deepstrike._kernel import Message, ToolCall, ToolSchema
from .base import ProviderDescriptor, RenderedContext, RetryConfig, RuntimePolicy, normalize_tool_call, openai_cached_prompt_tokens, wire_request_extensions
from .openai import OpenAIProvider
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import MINIMAX_POLICIES as _MINIMAX_POLICIES, ANTHROPIC_VENDOR_PROFILES
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent

_MINIMAX_OPENAI_BASE = "https://api.minimaxi.com/v1"


class MiniMaxAnthropicProvider(AnthropicCompatibleProvider):
    """MiniMax over its Anthropic-compatible endpoint. Replay is carried as
    Anthropic ``native_blocks`` (thinking / text / tool_use).

    Deprecated: prefer ``minimax(protocol="anthropic")``. Data-driven via
    ``ANTHROPIC_VENDOR_PROFILES["minimax"]``; thin shim for backward compat / isinstance.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["minimax"], api_key, model, retry_config, base_url)


class MiniMaxOpenAIProvider(OpenAIProvider):
    """MiniMax over its OpenAI-compatible endpoint. Replay is carried as
    ``reasoning_content`` / ``reasoning_details`` (split reasoning); requests
    default to ``reasoning_split: true``."""

    def __init__(
        self,
        api_key: str,
        model: str = "MiniMax-M2.7",
        retry_config: RetryConfig | None = None,
        base_url: str = _MINIMAX_OPENAI_BASE,
    ):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _MINIMAX_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="minimax",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _reasoning_split_enabled(self, extensions: dict | None) -> bool:
        return (extensions or {}).get("reasoning_split") is not False

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        return self._reasoning_split_enabled(extensions)

    def _request_extensions(self, extensions: dict | None) -> dict:
        return {**(extensions or {}), "reasoning_split": self._reasoning_split_enabled(extensions)}

    def _remember_minimax_replay(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: object,
        reasoning_details: object,
        native_tool_calls: list | None,
    ) -> None:
        has_reasoning = isinstance(reasoning_content, str) and reasoning_content.strip()
        has_details = reasoning_details is not None
        if not has_reasoning and not has_details:
            return
        envelope: dict = {
            "schema_version": 2,
            "provider": "minimax",
            "protocol": "openai-chat",
            "model": self._model,
        }
        if has_reasoning:
            envelope["reasoning_content"] = reasoning_content
        if has_details:
            envelope["reasoning_details"] = reasoning_details
        if native_tool_calls:
            envelope["tool_calls"] = native_tool_calls
        self.remember_replay_fields(
            Message(role="assistant", content=content, tool_calls=tool_calls or None),
            envelope,
        )

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context, extensions)
        tool_defs = self._build_tools(tools)
        ext = self._request_extensions(extensions)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = wire_request_extensions(ext)
                resp = await self._client.chat.completions.create(
                    **request_extensions,
                    model=self._model,
                    messages=msgs,
                    tools=tool_defs,
                )
                self._circuit.record_success()

                choice = resp.choices[0].message
                content = choice.content or ""
                native_tool_calls = choice.tool_calls or []
                tool_calls: list[ToolCall] = []
                for tc in native_tool_calls:
                    normalized = normalize_tool_call(tc.id, tc.function.name, tc.function.arguments)
                    if normalized:
                        tool_calls.append(normalized)

                reasoning_content = _model_field(choice, "reasoning_content")
                reasoning_details = _model_field(choice, "reasoning_details")
                wire_tool_calls = [
                    {"id": tc.id, "type": "function", "function": {"name": tc.function.name, "arguments": tc.function.arguments}}
                    for tc in native_tool_calls
                ]
                self._remember_minimax_replay(content, tool_calls, reasoning_content, reasoning_details, wire_tool_calls)

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
                    await asyncio.sleep(self._retry.base_delay * (2 ** attempt))

        raise last_exc or RuntimeError("Complete failed")

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        msgs = self._build_messages(context, extensions)
        tool_defs = self._build_tools(tools)
        ext = self._request_extensions(extensions)
        expose_reasoning = (extensions or {}).get("expose_reasoning")
        tool_call_bufs: dict[int, dict] = {}
        emitted: set[int] = set()
        reasoning_content = ""
        reasoning_details = None
        final_text = ""
        final_tool_calls: list[ToolCall] = []

        request_extensions = {k: v for k, v in ext.items() if k not in {"model", "messages", "tools", "stream", "stream_options"}}
        stream = await self._client.chat.completions.create(
            **request_extensions,
            model=self._model,
            messages=msgs,
            tools=tool_defs,
            stream=True,
            stream_options={"include_usage": True},
        )

        async for chunk in stream:
            if getattr(chunk, "usage", None):
                u = chunk.usage
                yield UsageEvent(
                    total_tokens=getattr(u, "total_tokens", 0) or 0,
                    input_tokens=getattr(u, "prompt_tokens", 0) or 0,
                    output_tokens=getattr(u, "completion_tokens", 0) or 0,
                    cache_read_input_tokens=openai_cached_prompt_tokens(u),
                )
                continue
            choice = chunk.choices[0] if chunk.choices else None
            if not choice:
                continue
            delta = getattr(choice, "delta", None)
            if not delta:
                continue
            native_reasoning = _model_field(delta, "reasoning_content")
            if native_reasoning:
                reasoning_content += str(native_reasoning)
                if expose_reasoning:
                    yield ThinkingDelta(delta=native_reasoning)
            details = _model_field(delta, "reasoning_details")
            if details is not None:
                reasoning_details = details
            if delta.content:
                final_text += delta.content
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
                    if idx in emitted:
                        continue
                    try:
                        args = json.loads(tb["args_buf"] or "{}")
                    except json.JSONDecodeError:
                        args = {}
                    tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
                    if tc_obj:
                        final_tool_calls.append(tc_obj)
                        emitted.add(idx)
                        yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)

        for idx, tb in tool_call_bufs.items():
            if idx in emitted:
                continue
            try:
                args = json.loads(tb["args_buf"] or "{}")
            except json.JSONDecodeError:
                args = {}
            tc_obj = normalize_tool_call(tb["id"], tb["name"], args)
            if tc_obj:
                final_tool_calls.append(tc_obj)
                yield ToolCallEvent(id=tc_obj.id, name=tc_obj.name, arguments=args)

        native_tool_calls = [
            {"id": tb["id"], "type": "function", "function": {"name": tb["name"], "arguments": tb["args_buf"] or "{}"}}
            for tb in tool_call_bufs.values()
        ]
        self._remember_minimax_replay(final_text, final_tool_calls, reasoning_content, reasoning_details, native_tool_calls)


def _model_field(obj: object, name: str):
    value = getattr(obj, name, None)
    if value is not None:
        return value
    extra = getattr(obj, "model_extra", None)
    if isinstance(extra, dict):
        return extra.get(name)
    return None
