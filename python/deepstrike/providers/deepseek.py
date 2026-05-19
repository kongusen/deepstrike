from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, RenderedContext, RuntimePolicy, normalize_tool_call
from .openai import OpenAIProvider

logger = logging.getLogger(__name__)

_DEEPSEEK_BASE_URL = "https://api.deepseek.com/v1"
_REASONER_MODELS = {"deepseek-reasoner", "deepseek-r1"}

_DEEPSEEK_POLICIES: dict[str, RuntimePolicy] = {
    "deepseek-chat":     RuntimePolicy(max_turns=25),
    "deepseek-reasoner": RuntimePolicy(max_turns=50),
    "deepseek-r1":       RuntimePolicy(max_turns=50),
    "deepseek-v4-flash": RuntimePolicy(max_turns=20),
    "deepseek-v4-pro":   RuntimePolicy(max_turns=35),
}


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

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        body = super()._build_body(messages, tools, stream)
        # reasoner models don't support tool calling
        if self._model in _REASONER_MODELS:
            body.pop("tools", None)
            body.pop("tool_choice", None)
        return body

    async def _stream_gen(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        ext = extensions or {}
        tool_calls: dict[int, dict] = {}
        emitted_tool_call_indexes: set[int] = set()
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
                        if ext.get("expose_reasoning"):
                            yield ThinkingDelta(delta=reasoning)
                    if text := delta.get("content"):
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
                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

    def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        messages = self._build_messages(context)
        return self._stream_gen(messages, tools, extensions)
