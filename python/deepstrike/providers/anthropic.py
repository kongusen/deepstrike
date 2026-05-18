from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from anthropic import AsyncAnthropic
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, RenderedContext, normalize_tool_call, to_anthropic_messages

logger = logging.getLogger(__name__)


class AnthropicProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "claude-sonnet-4-6",
        retry_config: RetryConfig | None = None,
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        self._client = AsyncAnthropic(api_key=api_key)
        self._native_assistant_blocks: dict[str, list[dict]] = {}

    def _build_messages(self, turns: list[Message]) -> list[dict]:
        return to_anthropic_messages(
            turns,
            native_replay=lambda message: self._native_assistant_blocks.get(
                self._assistant_replay_key(message)
            ),
        )

    def _build_tools(self, tools: list[ToolSchema]) -> list[dict] | None:
        if not tools:
            return None
        return [
            {
                "name": t.name,
                "description": t.description,
                "input_schema": json.loads(t.parameters),
            }
            for t in tools
        ]

    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message:
        if self._circuit.is_open():
            raise RuntimeError("Circuit breaker open")

        msgs = self._build_messages(context.turns)
        system = context.system_text or None
        tool_defs = self._build_tools(tools)

        last_exc = None
        for attempt in range(self._retry.max_retries):
            try:
                request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "system", "tools", "stream", "max_tokens"}}
                resp = await self._client.messages.create(
                    **request_extensions,
                    model=self._model,
                    max_tokens=(extensions or {}).get("max_tokens", 8096),
                    system=system,
                    messages=msgs,
                    tools=tool_defs,
                )
                self._circuit.record_success()

                content = ""
                tool_calls: list[ToolCall] = []
                native_blocks: list[dict] = []

                for block in resp.content:
                    if block.type == "text":
                        content += block.text
                        native_blocks.append({"type": "text", "text": block.text})
                    elif block.type == "tool_use":
                        tc = normalize_tool_call(block.id, block.name, block.input)
                        if tc:
                            tool_calls.append(tc)
                        native_blocks.append({
                            "type": "tool_use",
                            "id": block.id,
                            "name": block.name,
                            "input": block.input,
                        })
                    elif block.type == "thinking":
                        native_blocks.append({
                            "type": "thinking",
                            "thinking": block.thinking,
                            "signature": getattr(block, "signature", None),
                        })

                if tool_calls:
                    self._native_assistant_blocks[
                        self._assistant_replay_key_parts(content, tool_calls)
                    ] = native_blocks

                return Message(
                    role="assistant",
                    content=content,
                    token_count=resp.usage.input_tokens + resp.usage.output_tokens,
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
        msgs = self._build_messages(context.turns)
        system = context.system_text or None
        tool_defs = self._build_tools(tools)

        request_extensions = {k: v for k, v in (extensions or {}).items() if k not in {"model", "messages", "system", "tools", "stream", "max_tokens"}}
        async with self._client.messages.stream(
            **request_extensions,
            model=self._model,
            max_tokens=(extensions or {}).get("max_tokens", 8096),
            system=system,
            messages=msgs,
            tools=tool_defs,
        ) as stream:
            async for event in stream:
                if event.type == "content_block_delta":
                    delta = event.delta
                    if delta.type == "text_delta":
                        yield TextDelta(delta=delta.text)
                    elif delta.type == "thinking_delta":
                        yield ThinkingDelta(delta=delta.thinking)
                    elif delta.type == "input_json_delta":
                        pass
                elif event.type == "content_block_stop":
                    if hasattr(event, "content_block") and event.content_block.type == "tool_use":
                        block = event.content_block
                        tc = normalize_tool_call(block.id, block.name, block.input)
                        if tc:
                            yield ToolCallEvent(id=tc.id, name=tc.name, arguments=json.loads(tc.arguments))

    def _assistant_replay_key(self, message: Message) -> str:
        return self._assistant_replay_key_parts(message.content, message.tool_calls or [])

    def _assistant_replay_key_parts(self, content: str, tool_calls: list[ToolCall]) -> str:
        return json.dumps({
            "content": content,
            "tool_calls": [
                {"id": tc.id, "name": tc.name, "arguments": tc.arguments}
                for tc in tool_calls
            ],
        }, sort_keys=True)
