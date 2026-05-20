from __future__ import annotations
import json
import logging
from typing import AsyncIterator
import google.generativeai as genai
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, RuntimePolicy, normalize_tool_call

logger = logging.getLogger(__name__)

_GEMINI_POLICIES: dict[str, RuntimePolicy] = {
    "gemini-3-pro-preview": RuntimePolicy(max_turns=50),
    "gemini-3-flash-preview": RuntimePolicy(max_turns=25),
    "gemini-3.5-flash": RuntimePolicy(max_turns=30),
    "gemini-2.5-pro":        RuntimePolicy(max_turns=35),
    "gemini-2.5-flash":      RuntimePolicy(max_turns=20),
    "gemini-2.0-flash":      RuntimePolicy(max_turns=15),
    "gemini-2.0-flash-lite": RuntimePolicy(max_turns=10),
    "gemini-1.5-pro":        RuntimePolicy(max_turns=30),
    "gemini-1.5-flash":      RuntimePolicy(max_turns=15),
}


class GeminiProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "gemini-2.0-flash",
        retry_config: RetryConfig | None = None,
        base_url: str = "https://generativelanguage.googleapis.com",
    ):
        self._model_name = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._base_url = base_url.rstrip("/")
        genai.configure(api_key=api_key, client_options={"api_endpoint": self._base_url})
        self._model = genai.GenerativeModel(model)

    def runtime_policy(self) -> RuntimePolicy:
        return _GEMINI_POLICIES.get(self._model_name, RuntimePolicy())

    def _build_contents(self, turns: list[Message]) -> list[dict]:
        contents = []
        for msg in turns:
            if msg.role == "tool":
                parts = []
                for p in getattr(msg, "content_parts", []):
                    if p.type == "tool_result":
                        tool_name = p.call_id
                        for turn in reversed(turns):
                            if turn.role == "assistant" and turn.tool_calls:
                                matched = next((tc for tc in turn.tool_calls if tc.id == p.call_id), None)
                                if matched:
                                    tool_name = matched.name
                                    break
                        parts.append({
                            "function_response": {
                                "name": tool_name,
                                "response": {"output": p.output},
                            }
                        })
                if parts:
                    contents.append({"role": "user", "parts": parts})
                continue

            role = "model" if msg.role == "assistant" else "user"
            parts = []
            if msg.tool_calls:
                for tc in msg.tool_calls:
                    try:
                        args = json.loads(tc.arguments)
                    except json.JSONDecodeError:
                        args = {}
                    parts.append({"function_call": {"name": tc.name, "args": args}})
            if msg.content:
                parts.append({"text": msg.content})
            if parts:
                contents.append({"role": role, "parts": parts})
        return contents

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        if not tools:
            return None
        return [
            {
                "name": t.name,
                "description": t.description,
                "parameters": json.loads(t.parameters),
            }
            for t in tools
        ]

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        system = context.system_text or None
        contents = self._build_contents(context.turns)
        tool_defs = self._build_tools(tools)

        if system:
            self._model = genai.GenerativeModel(self._model_name, system_instruction=system)
        if tool_defs:
            self._model = genai.GenerativeModel(self._model_name, tools=tool_defs)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                resp = await self._model.generate_content_async(contents)
                self._circuit.record_success()

                content = ""
                tool_calls: list[ToolCall] = []

                for part in resp.candidates[0].content.parts:
                    if hasattr(part, "text"):
                        content += part.text
                    elif hasattr(part, "function_call"):
                        fc = part.function_call
                        tc = normalize_tool_call(fc.name, fc.name, dict(fc.args))
                        if tc:
                            tool_calls.append(tc)

                return Message(
                    role="assistant",
                    content=content,
                    token_count=resp.usage_metadata.total_token_count if resp.usage_metadata else None,
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
        system = context.system_text or None
        contents = self._build_contents(context.turns)
        tool_defs = self._build_tools(tools)

        if system:
            self._model = genai.GenerativeModel(self._model_name, system_instruction=system)
        if tool_defs:
            self._model = genai.GenerativeModel(self._model_name, tools=tool_defs)

        tool_calls: list[dict] = []

        async for chunk in await self._model.generate_content_async(contents, stream=True):
            for part in chunk.candidates[0].content.parts if chunk.candidates else []:
                if hasattr(part, "text"):
                    yield TextDelta(delta=part.text)
                elif hasattr(part, "function_call"):
                    fc = part.function_call
                    tool_calls.append({"id": f"call_{len(tool_calls) + 1}", "name": fc.name, "args": dict(fc.args)})

        for tc in tool_calls:
            yield ToolCallEvent(id=tc["id"], name=tc["name"], arguments=tc["args"])
