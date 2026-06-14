from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, wire_request_extensions
from .anthropic import AnthropicProvider
from .openai import OpenAIProvider

logger = logging.getLogger(__name__)

_DEEPSEEK_ANTHROPIC_BASE = "https://api.deepseek.com/anthropic"
_DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1"
_REASONER_MODELS = {"deepseek-reasoner", "deepseek-r1"}
# Models whose tool turns require a non-empty reasoning_content replay (fail fast
# rather than send a request the provider rejects with 400).
_DEEPSEEK_REASONING_MODELS = {"deepseek-reasoner", "deepseek-r1", "deepseek-v4-flash", "deepseek-v4-pro"}

_DEEPSEEK_POLICIES: dict[str, RuntimePolicy] = {
    "deepseek-chat":     RuntimePolicy(max_turns=25),
    "deepseek-reasoner": RuntimePolicy(max_turns=50),
    "deepseek-r1":       RuntimePolicy(max_turns=50),
    "deepseek-v4-flash": RuntimePolicy(max_turns=20),
    "deepseek-v4-pro":   RuntimePolicy(max_turns=35),
}


class DeepSeekAnthropicProvider(AnthropicProvider):
    """DeepSeek over its Anthropic-compatible endpoint."""

    def __init__(
        self,
        api_key: str,
        model: str = "deepseek-v4-flash",
        retry_config: RetryConfig | None = None,
        base_url: str = _DEEPSEEK_ANTHROPIC_BASE,
    ):
        super().__init__(api_key, model, retry_config, base_url=base_url)

    def _provider_name(self) -> str:
        return "deepseek"

    def runtime_policy(self) -> RuntimePolicy:
        return _DEEPSEEK_POLICIES.get(self._model, RuntimePolicy())


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

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        body = super()._build_body(messages, tools, stream)
        if self._model in _REASONER_MODELS:
            body.pop("tools", None)
            body.pop("tool_choice", None)
        return body

    async def _stream_gen(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        ext = extensions or {}
        tool_calls: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
        reasoning_content = ""
        final_text = ""
        final_tool_calls: list[ToolCall] = []
        async with httpx.AsyncClient() as client:
            async with client.stream("POST", f"{self._base_url}/chat/completions",
                                     headers=self._headers(),
                                     json=self._build_body(messages, tools, stream=True),
                                     timeout=120) as resp:
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
                    if reasoning := delta.get("reasoning_content"):
                        reasoning_content += str(reasoning)
                        if ext.get("expose_reasoning"):
                            yield ThinkingDelta(delta=reasoning)
                    if text := delta.get("content"):
                        final_text += str(text)
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
                        for idx, tb in tool_calls.items():
                            if idx in emitted_tool_call_indexes:
                                continue
                            try:
                                args = json.loads(tb["args_buf"] or "{}")
                            except json.JSONDecodeError:
                                args = {}
                            tc = normalize_tool_call(tb["id"], tb["name"], args)
                            if tc:
                                final_tool_calls.append(tc)
                                emitted_tool_call_indexes.add(idx)
                                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

        for idx, tb in tool_calls.items():
            if idx in emitted_tool_call_indexes:
                continue
            try:
                args = json.loads(tb["args_buf"] or "{}")
            except json.JSONDecodeError:
                args = {}
            tc = normalize_tool_call(tb["id"], tb["name"], args)
            if tc:
                final_tool_calls.append(tc)
                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

        native_tool_calls = [
            {
                "id": tb["id"],
                "type": "function",
                "function": {"name": tb["name"], "arguments": tb["args_buf"] or "{}"},
            }
            for tb in tool_calls.values()
        ]
        self._remember_deepseek_replay(final_text, final_tool_calls, reasoning_content, native_tool_calls)

    def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        messages = self._build_messages(context, extensions)
        return self._stream_gen(messages, tools, extensions)
