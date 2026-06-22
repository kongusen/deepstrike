from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from .base import RetryConfig, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, openai_cached_prompt_tokens, wire_request_extensions
from .openai import OpenAIProvider
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import DEEPSEEK_POLICIES as _DEEPSEEK_POLICIES, ANTHROPIC_VENDOR_PROFILES

logger = logging.getLogger(__name__)

_DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1"
_REASONER_MODELS = {"deepseek-reasoner", "deepseek-r1"}
# Models whose tool turns require a non-empty reasoning_content replay (fail fast
# rather than send a request the provider rejects with 400).
_DEEPSEEK_REASONING_MODELS = {"deepseek-reasoner", "deepseek-r1", "deepseek-v4-flash", "deepseek-v4-pro"}


class DeepSeekAnthropicProvider(AnthropicCompatibleProvider):
    """DeepSeek over its Anthropic-compatible endpoint.

    Deprecated: prefer ``deepseek(protocol="anthropic")``. Behavior is data-driven
    via ``ANTHROPIC_VENDOR_PROFILES["deepseek"]``; this thin shim is kept for
    backward compatibility and ``isinstance`` checks.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["deepseek"], api_key, model, retry_config, base_url)


class DeepSeekProvider(OpenAIProvider):
    """DeepSeek provider.

    deepseek-chat: full tool calling
    deepseek-reasoner / deepseek-r1: chain-of-thought via reasoning_content, NO tool calling

    extensions:
      expose_reasoning (bool): prepend <think>…</think> to content
    """

    def __init__(self, api_key: str, model: str = "deepseek-chat", retry_config: RetryConfig | None = None, base_url: str = _DEEPSEEK_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _DEEPSEEK_POLICIES.get(self._model, RuntimePolicy())

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider="deepseek",
            protocol="openai-chat",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def _require_non_empty_reasoning_replay_for_tool_turns(self, extensions: dict | None) -> bool:
        if (extensions or {}).get("thinking") is False:
            return False
        return self._model in _DEEPSEEK_REASONING_MODELS

    def _remember_deepseek_replay(
        self,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: object,
        native_tool_calls: list | None = None,
    ) -> None:
        """Persist a provider-scoped replay envelope only when real reasoning was
        produced. A missing/empty reasoning_content is never synthesized."""
        if not (isinstance(reasoning_content, str) and reasoning_content.strip()):
            return
        envelope: dict = {
            "schema_version": 2,
            "provider": "deepseek",
            "protocol": "openai-chat",
            "model": self._model,
            "reasoning_content": reasoning_content,
        }
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

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = wire_request_extensions(extensions)
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

                reasoning_content = getattr(choice, "reasoning_content", None)
                if reasoning_content is None and getattr(choice, "model_extra", None):
                    reasoning_content = choice.model_extra.get("reasoning_content")
                wire_tool_calls = [
                    {"id": tc.id, "type": "function", "function": {"name": tc.function.name, "arguments": tc.function.arguments}}
                    for tc in native_tool_calls
                ]
                self._remember_deepseek_replay(content, tool_calls, reasoning_content, wire_tool_calls)

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
        ext = extensions or {}
        msgs = self._build_messages(context, extensions)
        # Reasoner models (deepseek-reasoner / deepseek-r1) do not support tool calling.
        tool_defs = None if self._model in _REASONER_MODELS else self._build_tools(tools)
        expose_reasoning = ext.get("expose_reasoning")
        tool_call_bufs: dict[int, dict] = {}
        emitted: set[int] = set()
        reasoning_content = ""
        final_text = ""
        final_tool_calls: list[ToolCall] = []

        # Unlike the base provider we never add prompt_cache_key: DeepSeek 400s on unknown params and
        # auto prefix-caches anyway (mirrors complete() and the Node DeepSeekProvider cacheKeyParams).
        request_extensions = {k: v for k, v in wire_request_extensions(ext).items() if k not in {"model", "messages", "tools", "stream", "stream_options"}}
        create_kwargs: dict = {**request_extensions, "model": self._model, "messages": msgs, "stream": True, "stream_options": {"include_usage": True}}
        if tool_defs:
            create_kwargs["tools"] = tool_defs
        stream = await self._client.chat.completions.create(**create_kwargs)

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
            native_reasoning = _delta_field(delta, "reasoning_content")
            if native_reasoning:
                reasoning_content += str(native_reasoning)
                if expose_reasoning:
                    yield ThinkingDelta(delta=native_reasoning)
            if delta.content:
                final_text += str(delta.content)
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
        self._remember_deepseek_replay(final_text, final_tool_calls, reasoning_content, native_tool_calls)


def _delta_field(obj: object, name: str):
    """Read a (possibly non-standard) field off a streamed delta — direct attr first, then the
    openai SDK's ``model_extra`` bag where vendor extensions like reasoning_content land."""
    value = getattr(obj, name, None)
    if value is not None:
        return value
    extra = getattr(obj, "model_extra", None)
    if isinstance(extra, dict):
        return extra.get(name)
    return None
