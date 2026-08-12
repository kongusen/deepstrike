"""Anthropic Messages protocol lifecycle adapter."""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from deepstrike._kernel import Message, ToolCall, ToolSchema
from deepstrike.providers.base import normalize_tool_call, parse_tool_arguments, to_anthropic_messages
from deepstrike.providers.protocol_adapter import AdapterOutput, ProtocolResponseError
from deepstrike.providers.stop_reason import canonicalize_stop_reason
from deepstrike.providers.stream import TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from deepstrike.providers.usage import ProviderUsage
from deepstrike.types.content import CanonicalAdapterInput


def _get(raw: Any, key: str) -> Any:
    return raw.get(key) if isinstance(raw, dict) else getattr(raw, key, None)


def _number(raw: Any, field: str) -> int | None:
    value = _get(raw, field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ProtocolResponseError("anthropic-messages", f"usage.{field} is invalid")
    return int(value)


@dataclass(frozen=True)
class AnthropicRequestPlan:
    transport: str
    params: dict[str, Any]
    cache_slots: dict[str, bool]


@dataclass
class AnthropicStreamState:
    native_blocks: dict[int, dict] = field(default_factory=dict)
    tool_blocks: dict[int, dict] = field(default_factory=dict)
    final_tool_calls: list[ToolCall] = field(default_factory=list)
    final_text: str = ""
    uncached_input: int = 0
    cache_read: int = 0
    cache_creation: int = 0
    output_tokens: int = 0
    cache_slots: dict[str, bool] = field(default_factory=dict)


class AnthropicMessagesAdapter:
    protocol = "anthropic-messages"

    def __init__(self, model: str = "claude-sonnet-4-6") -> None:
        self._model = model

    def build_request(
        self,
        input: CanonicalAdapterInput,
        *,
        messages: list[dict] | None = None,
        system: Any = None,
        tools: list[dict] | None = None,
    ) -> AnthropicRequestPlan:
        context = input.context
        if messages is None:
            messages = to_anthropic_messages(context.turns, resolved=input.resolved)
        if not messages:
            messages = [{"role": "user", "content": "Proceed."}]
        if system is None:
            stable = getattr(context, "system_stable", "") or ""
            knowledge = getattr(context, "system_knowledge", "") or ""
            if stable or knowledge:
                cache_control = {"type": "ephemeral", **({"ttl": "1h"} if input.extensions.get("cacheTtl") == "1h" else {})}
                system = [
                    {"type": "text", "text": text, "cache_control": cache_control}
                    for text in (stable, knowledge) if text
                ]
            else:
                system = context.system_text or None
        if tools is None and input.tools:
            tools = [{"name": tool.name, "description": tool.description, "input_schema": json.loads(tool.parameters)} for tool in input.tools]
        ext = {key: value for key, value in input.extensions.items() if key not in {"model", "messages", "system", "tools", "stream", "max_tokens", "cacheBreakpointStrategy", "cacheTtl"}}
        betas = input.extensions.get("betas")
        if isinstance(betas, list) and betas:
            ext["betas"] = betas
        params = {
            **ext,
            "model": input.resolved.model_id if input.resolved is not None else self._model,
            "max_tokens": input.extensions.get("max_tokens", 8096),
            **({"system": system} if system else {}),
            "messages": messages,
            **({"tools": tools} if tools else {}),
        }
        return AnthropicRequestPlan(
            transport="beta" if betas else "stable",
            params=params,
            cache_slots={
                "system": isinstance(system, list) and any(isinstance(block, dict) and block.get("cache_control") for block in system),
                "tools": bool(tools) and any(isinstance(tool, dict) and tool.get("cache_control") for tool in tools or []),
                "messages": any(isinstance(message.get("content"), list) and any(block.get("cache_control") for block in message["content"] if isinstance(block, dict)) for message in messages),
            },
        )

    def decode_complete(self, raw: Any, input: CanonicalAdapterInput) -> tuple[Message, dict | None]:
        content = ""
        calls: list[ToolCall] = []
        native_blocks: list[dict] = []
        for block in _get(raw, "content") or []:
            kind = _get(block, "type")
            if kind == "text":
                text = _get(block, "text") or ""
                content += text
                native_blocks.append({"type": "text", "text": text})
            elif kind == "tool_use":
                call = normalize_tool_call(_get(block, "id"), _get(block, "name"), _get(block, "input"))
                if call:
                    calls.append(call)
                native_blocks.append({"type": "tool_use", "id": _get(block, "id"), "name": _get(block, "name"), "input": _get(block, "input") or {}})
            elif kind == "thinking":
                native_blocks.append({"type": "thinking", "thinking": _get(block, "thinking"), "signature": _get(block, "signature")})
        usage = self.normalize_usage(_get(raw, "usage"))
        token_count = usage.input_tokens + usage.output_tokens if usage else None
        message = Message(role="assistant", content=content, token_count=token_count, tool_calls=calls or None)
        return message, ({"native_blocks": native_blocks} if native_blocks else None)

    def create_stream_state(self, input: CanonicalAdapterInput, cache_slots: dict[str, bool] | None = None) -> AnthropicStreamState:
        return AnthropicStreamState(cache_slots=dict(cache_slots or {}))

    def push_stream_chunk(self, chunk: Any, state: AnthropicStreamState) -> AdapterOutput:
        events = []
        kind = _get(chunk, "type")
        if kind in ("message_start", "message_delta"):
            usage = _get(chunk, "usage") or _get(_get(chunk, "message"), "usage")
            if usage is not None:
                state.uncached_input = max(state.uncached_input, _number(usage, "input_tokens") or 0)
                state.cache_read = max(state.cache_read, _number(usage, "cache_read_input_tokens") or 0)
                state.cache_creation = max(state.cache_creation, _number(usage, "cache_creation_input_tokens") or 0)
                state.output_tokens = max(state.output_tokens, _number(usage, "output_tokens") or 0)
                raw_stop = _get(_get(chunk, "delta"), "stop_reason")
                total_input = state.uncached_input + state.cache_read + state.cache_creation
                provider_usage = ProviderUsage(input_tokens=total_input, output_tokens=state.output_tokens, cache_read_input_tokens=state.cache_read, cache_creation_input_tokens=state.cache_creation)
                contributors = [key for key in ("system", "tools", "messages") if state.cache_slots.get(key)]
                by_slot = None
                if state.cache_read and contributors:
                    share, remainder = divmod(state.cache_read, len(contributors))
                    by_slot = {key: share + (remainder if index == 0 else 0) for index, key in enumerate(contributors)}
                events.append(UsageEvent(total_tokens=total_input + state.output_tokens, input_tokens=total_input, output_tokens=state.output_tokens, cache_read_input_tokens=state.cache_read, cache_creation_input_tokens=state.cache_creation, cache_read_input_tokens_by_slot=by_slot, stop_reason=canonicalize_stop_reason(raw_stop), raw_stop_reason=raw_stop, provider_usage=provider_usage))
        elif kind == "content_block_start":
            idx = int(_get(chunk, "index"))
            block = _get(chunk, "content_block")
            block_type = _get(block, "type")
            state.native_blocks[idx] = {"type": block_type}
            if block_type == "thinking":
                state.native_blocks[idx].update({"thinking": _get(block, "thinking") or "", "signature": _get(block, "signature") or ""})
            elif block_type == "text":
                state.native_blocks[idx]["text"] = _get(block, "text") or ""
            elif block_type == "tool_use":
                state.native_blocks[idx].update({"id": _get(block, "id"), "name": _get(block, "name"), "input": _get(block, "input") or {}})
                state.tool_blocks[idx] = {"id": _get(block, "id"), "name": _get(block, "name"), "args": ""}
        elif kind == "content_block_delta":
            idx = int(_get(chunk, "index"))
            delta = _get(chunk, "delta")
            delta_type = _get(delta, "type")
            if delta_type == "text_delta":
                text = _get(delta, "text") or ""
                state.final_text += text
                state.native_blocks.setdefault(idx, {"type": "text"})["text"] = state.native_blocks.get(idx, {}).get("text", "") + text
                events.append(TextDelta(delta=text))
            elif delta_type == "thinking_delta":
                text = _get(delta, "thinking") or ""
                state.native_blocks.setdefault(idx, {"type": "thinking"})["thinking"] = state.native_blocks.get(idx, {}).get("thinking", "") + text
                events.append(ThinkingDelta(delta=text))
            elif delta_type == "signature_delta":
                state.native_blocks.setdefault(idx, {"type": "thinking"})["signature"] = _get(delta, "signature") or ""
            elif delta_type == "input_json_delta" and idx in state.tool_blocks:
                state.tool_blocks[idx]["args"] += _get(delta, "partial_json") or ""
        elif kind == "content_block_stop":
            idx = int(_get(chunk, "index"))
            if idx in state.tool_blocks:
                block = state.tool_blocks.pop(idx)
                try:
                    args = json.loads(block["args"] or "{}")
                except json.JSONDecodeError:
                    args = {}
                state.native_blocks[idx]["input"] = args
                call = normalize_tool_call(block["id"], block["name"], args)
                if call:
                    state.final_tool_calls.append(call)
                    events.append(ToolCallEvent(id=call.id, name=call.name, arguments=args))
        return AdapterOutput(events=events)

    def finish_stream(self, state: AnthropicStreamState, final: Any = None) -> AdapterOutput:
        blocks = [state.native_blocks[index] for index in sorted(state.native_blocks)]
        return AdapterOutput(events=[], replay={"native_blocks": blocks} if blocks else None)

    def normalize_usage(self, raw: Any) -> ProviderUsage | None:
        if raw is None:
            return None
        if not isinstance(raw, dict) and not hasattr(raw, "input_tokens"):
            raise ProtocolResponseError("anthropic-messages", "usage must be an object")
        input_tokens = _number(raw, "input_tokens")
        cache_read = _number(raw, "cache_read_input_tokens")
        cache_creation = _number(raw, "cache_creation_input_tokens")
        output = _number(raw, "output_tokens")
        if input_tokens is None and cache_read is None and cache_creation is None and output is None:
            return None
        return ProviderUsage(input_tokens=(input_tokens or 0) + (cache_read or 0) + (cache_creation or 0), output_tokens=output or 0, cache_read_input_tokens=cache_read or 0, cache_creation_input_tokens=cache_creation or 0)
