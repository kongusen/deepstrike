from __future__ import annotations
import json
import logging
from typing import AsyncIterator
from anthropic import AsyncAnthropic
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent
from .base import RetryConfig, CircuitBreaker, ProviderDescriptor, RenderedContext, RuntimePolicy, normalize_tool_call, parse_tool_arguments, to_anthropic_messages

logger = logging.getLogger(__name__)

_CLAUDE_POLICIES: dict[str, RuntimePolicy] = {
    "claude-opus-4-1":           RuntimePolicy(max_turns=50),
    "claude-opus-4-7":           RuntimePolicy(max_turns=50),
    "claude-opus-4-6":           RuntimePolicy(max_turns=50),
    "claude-opus-4-0":           RuntimePolicy(max_turns=50),
    "claude-sonnet-4-6":         RuntimePolicy(max_turns=25),
    "claude-sonnet-4-0":         RuntimePolicy(max_turns=25),
    "claude-haiku-4-5":          RuntimePolicy(max_turns=15),
    "claude-3-5-haiku-latest":   RuntimePolicy(max_turns=15),
}


class AnthropicProvider:
    def __init__(
        self,
        api_key: str,
        model: str = "claude-sonnet-4-6",
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        self._model = model
        self._retry = retry_config or RetryConfig()
        self._circuit = CircuitBreaker(self._retry)
        client_kwargs: dict = {"api_key": api_key}
        if base_url:
            client_kwargs["base_url"] = base_url
        self._client = AsyncAnthropic(**client_kwargs)
        self._native_assistant_blocks: dict[str, list[dict]] = {}

    def runtime_policy(self) -> RuntimePolicy:
        return _CLAUDE_POLICIES.get(self._model, RuntimePolicy())

    def _provider_name(self) -> str:
        """Identity advertised in the descriptor; overridden by Anthropic-compatible vendors (e.g. MiniMax)."""
        return "anthropic"

    def descriptor(self) -> ProviderDescriptor:
        return ProviderDescriptor(
            provider=self._provider_name(),
            protocol="anthropic-messages",
            model=self._model,
            reasoning={"supported": True, "preserve_across_tool_turns": True, "requires_replay_for_tool_turns": True},
            tool_calls={"supported": True, "requires_strict_pairing": True},
        )

    def peek_provider_replay(self, content: str, tool_calls: list[ToolCall]) -> dict | None:
        blocks = self._native_assistant_blocks.get(self._assistant_replay_key_parts(content, tool_calls))
        return {"native_blocks": blocks} if blocks else None

    def seed_provider_replay(self, content: str, tool_calls: list[ToolCall], replay: dict) -> None:
        blocks = replay.get("native_blocks")
        if blocks:
            self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks
            return
        # Legacy log without persisted native blocks: reconstruct neutral
        # text + tool_use blocks from the transcript so a tool-use turn can be
        # replayed. Thinking blocks were never persisted and are not recovered.
        reconstructed = _reconstruct_anthropic_blocks(content, tool_calls)
        if reconstructed:
            self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = reconstructed

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

    def _remember_native_blocks(self, content: str, tool_calls: list[ToolCall], blocks: list[dict]) -> None:
        if not blocks:
            return
        if not tool_calls and not any(b.get("type") == "thinking" for b in blocks):
            return
        self._native_assistant_blocks[self._assistant_replay_key_parts(content, tool_calls)] = blocks

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

                self._remember_native_blocks(content, tool_calls, native_blocks)

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

    async def stream(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None, state: dict | None = None) -> AsyncIterator[StreamEvent]:
        msgs = self._build_messages(context.turns)
        system = context.system_text or None
        tool_defs = self._build_tools(tools)

        native_blocks: dict[int, dict] = {}
        tool_blocks: dict[int, dict] = {}
        final_text = ""
        final_tool_calls: list[ToolCall] = []

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
                if event.type in ("message_start", "message_delta"):
                    usage = getattr(event, "usage", None) or getattr(getattr(event, "message", None), "usage", None)
                    if usage is not None:
                        from deepstrike.providers.stream import UsageEvent
                        yield UsageEvent(total_tokens=usage.input_tokens + getattr(usage, "output_tokens", 0))
                elif event.type == "content_block_start":
                    idx = event.index
                    block = event.content_block
                    native_blocks[idx] = {"type": block.type}
                    if block.type == "thinking":
                        native_blocks[idx]["thinking"] = getattr(block, "thinking", "") or ""
                        native_blocks[idx]["signature"] = getattr(block, "signature", "") or ""
                    elif block.type == "text":
                        native_blocks[idx]["text"] = getattr(block, "text", "") or ""
                    elif block.type == "tool_use":
                        native_blocks[idx].update({
                            "id": block.id,
                            "name": block.name,
                            "input": getattr(block, "input", {}) or {},
                        })
                        tool_blocks[idx] = {"id": block.id, "name": block.name, "args_buf": ""}
                elif event.type == "content_block_delta":
                    delta = event.delta
                    idx = event.index
                    if delta.type == "text_delta":
                        final_text += delta.text
                        native_blocks[idx]["text"] = native_blocks[idx].get("text", "") + delta.text
                        yield TextDelta(delta=delta.text)
                    elif delta.type == "thinking_delta":
                        native_blocks[idx]["thinking"] = native_blocks[idx].get("thinking", "") + delta.thinking
                        yield ThinkingDelta(delta=delta.thinking)
                    elif delta.type == "signature_delta":
                        native_blocks[idx]["signature"] = native_blocks[idx].get("signature", "") + delta.signature
                    elif delta.type == "input_json_delta" and idx in tool_blocks:
                        tool_blocks[idx]["args_buf"] += delta.partial_json
                elif event.type == "content_block_stop":
                    idx = event.index
                    if idx in tool_blocks:
                        tb = tool_blocks.pop(idx)
                        try:
                            args = json.loads(tb["args_buf"] or "{}")
                        except json.JSONDecodeError:
                            args = {}
                        native_blocks[idx]["input"] = args
                        tc = normalize_tool_call(tb["id"], tb["name"], args)
                        if tc:
                            final_tool_calls.append(tc)
                            yield ToolCallEvent(id=tc.id, name=tc.name, arguments=args)

        ordered_blocks = [native_blocks[i] for i in sorted(native_blocks)]
        self._remember_native_blocks(final_text, final_tool_calls, ordered_blocks)

    def _assistant_replay_key(self, message: Message) -> str:
        return self._assistant_replay_key_parts(message.content, message.tool_calls or [])

    def _assistant_replay_key_parts(self, content: str, tool_calls: list[ToolCall]) -> str:
        return json.dumps({
            "content": content,
            "tool_calls": [
                {"id": _tc_field(tc, "id"), "name": _tc_field(tc, "name"), "arguments": _tc_field(tc, "arguments")}
                for tc in tool_calls
            ],
        }, sort_keys=True)


def _tc_field(tc: object, field_name: str) -> object:
    if isinstance(tc, dict):
        return tc.get(field_name)
    return getattr(tc, field_name, None)


def _reconstruct_anthropic_blocks(content: str, tool_calls: list) -> list[dict]:
    """Reconstruct Anthropic assistant content blocks from a neutral transcript
    when no provider replay was persisted. Only meaningful for tool-use turns."""
    if not tool_calls:
        return []
    blocks: list[dict] = []
    if content:
        blocks.append({"type": "text", "text": content})
    for tc in tool_calls:
        blocks.append({
            "type": "tool_use",
            "id": _tc_field(tc, "id"),
            "name": _tc_field(tc, "name"),
            "input": parse_tool_arguments(_tc_field(tc, "arguments")),
        })
    return blocks
