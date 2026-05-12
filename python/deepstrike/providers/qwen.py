from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import httpx
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, normalize_tool_call
from .openai import OpenAIProvider

logger = logging.getLogger(__name__)

_DASHSCOPE_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1"


class QwenProvider(OpenAIProvider):
    """Qwen via DashScope OpenAI-compatible API.

    extensions:
      enable_thinking (bool): enable chain-of-thought for Qwen3 models
      thinking_budget (int): max tokens for thinking block
    """

    def __init__(self, api_key: str, model: str = "qwen-max", retry_config: RetryConfig | None = None, base_url: str = _DASHSCOPE_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def _build_body(self, messages: list[Message], tools: list[ToolSchema], stream: bool, extensions: dict | None = None) -> dict:
        body = super()._build_body(messages, tools, stream)
        ext = extensions or {}
        if "enable_thinking" in ext:
            body["enable_thinking"] = bool(ext["enable_thinking"])
            if "thinking_budget" in ext:
                body["thinking_budget"] = int(ext["thinking_budget"])
        return body

    async def _stream_gen(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        tool_calls: dict[int, dict] = {}
        async with httpx.AsyncClient() as client:
            async with client.stream("POST", f"{self._base_url}/chat/completions",
                                     headers=self._headers(),
                                     json=self._build_body(messages, tools, stream=True, extensions=extensions),
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
                        for tb in tool_calls.values():
                            try:
                                args = json.loads(tb["args_buf"] or "{}")
                            except json.JSONDecodeError:
                                args = {}
                            tc = normalize_tool_call(tb["id"], tb["name"], args)
                            if tc:
                                yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)
                        tool_calls.clear()

    async def stream(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]:
        return self._stream_gen(messages, tools, extensions)
