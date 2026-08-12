"""OpenAI-compatible chat protocol lifecycle shared by table-driven dialects."""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any, Callable

from deepstrike._kernel import Message, ToolCall, ToolSchema
from deepstrike.providers.base import (
    ThinkingTagStreamExtractor,
    openai_cached_prompt_tokens,
    stable_prompt_cache_key,
    normalize_tool_call,
    to_openai_message_params,
    wire_request_extensions,
)
from deepstrike.providers.protocol_adapter import AdapterOutput, ProtocolResponseError
from deepstrike.providers.replay import openai_chat_wire_replay_fields
from deepstrike.providers.replay_validator import (
    DEGRADED_REASONING_PLACEHOLDER,
    validate_openai_chat_replay,
)
from deepstrike.providers.stop_reason import canonicalize_stop_reason
from deepstrike.providers.stream import TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent
from deepstrike.providers.usage import normalize_usage
from deepstrike.types.content import CanonicalAdapterInput


@dataclass(frozen=True)
class OpenAIChatRequestPlan:
    params: dict[str, Any]
    prepared_extensions: dict[str, Any]
    dialect: Any


@dataclass(frozen=True)
class OpenAIChatDecodeResult:
    message: Message
    replay: dict[str, Any] | None = None


@dataclass
class OpenAIChatStreamState:
    input: CanonicalAdapterInput
    dialect: Any
    tool_call_buffers: dict[int, dict] = field(default_factory=dict)
    emitted_tool_call_indexes: set[int] = field(default_factory=set)
    extractor: ThinkingTagStreamExtractor = field(default_factory=ThinkingTagStreamExtractor)
    accumulated_reasoning: str = ""
    accumulated_reasoning_details: Any = None
    accumulated_content: str = ""
    total_tokens: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    cache_read_tokens: int = 0
    finish_reason: str | None = None
    raw_usage: Any = None


def _get(raw: Any, name: str) -> Any:
    return raw.get(name) if isinstance(raw, dict) else getattr(raw, name, None)


def _number(raw: Any, field: str) -> int | None:
    value = _get(raw, field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ProtocolResponseError("openai-chat", f"usage.{field} must be a non-negative number")
    return int(value)


class OpenAIChatAdapter:
    protocol = "openai-chat"

    def __init__(self, model: str = "gpt-4o") -> None:
        self._model = model

    def build_tools(self, tools: tuple[ToolSchema, ...] | list[ToolSchema]) -> list[dict]:
        return [{
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": json.loads(tool.parameters),
            },
        } for tool in tools]

    def _messages(
        self,
        input: CanonicalAdapterInput,
        dialect: Any,
        prepared: dict[str, Any],
        replay_for_assistant: Callable[[str, list[ToolCall]], dict | None] | None,
    ) -> list[dict]:
        require_reasoning = bool(dialect.require_reasoning_replay(prepared))
        degrade = prepared.get("degrade_missing_reasoning_replay") is True
        validate_openai_chat_replay(
            input.context.turns,
            require_non_empty_reasoning_for_tool_calls=require_reasoning,
            degrade_missing_reasoning=degrade,
            replay_for_assistant=replay_for_assistant,
        )
        messages = to_openai_message_params(input.context, input.resolved)
        cursor = 1 if input.context.system_text else 0
        for message in input.context.turns:
            if message.role == "tool":
                cursor += sum(1 for part in (getattr(message, "content_parts", None) or []) if part.type == "tool_result")
                continue
            if message.role == "assistant" and getattr(message, "tool_calls", None):
                replay = replay_for_assistant(message.content, message.tool_calls) if replay_for_assistant else None
                fields = openai_chat_wire_replay_fields(replay)
                if not fields and require_reasoning and degrade:
                    fields = {"reasoning_content": DEGRADED_REASONING_PLACEHOLDER}
                if fields:
                    messages[cursor] = {**messages[cursor], **fields}
            cursor += 1
        if getattr(dialect, "supports_context_cache", False):
            cache_message = self._context_cache_message(input.extensions)
            if cache_message is not None:
                messages.insert(0, cache_message)
        return messages

    @staticmethod
    def _context_cache_message(extensions: dict[str, Any]) -> dict | None:
        cache_id = extensions.get("context_cache_id")
        cache_tag = extensions.get("context_cache_tag")
        if cache_id:
            reference = f"cache_id={cache_id}"
        elif cache_tag:
            reference = f"tag={cache_tag}"
        else:
            return None
        if extensions.get("context_cache_reset_ttl") is not None:
            reference += f";reset_ttl={int(extensions['context_cache_reset_ttl'])}"
        return {"role": "cache", "content": reference}

    def build_request(
        self,
        input: CanonicalAdapterInput,
        dialect: Any,
        replay_for_assistant: Callable[[str, list[ToolCall]], dict | None] | None = None,
    ) -> OpenAIChatRequestPlan:
        prepared = dict(dialect.prepare_extensions(input.extensions) if dialect.prepare_extensions else input.extensions)
        tools = [] if input.resolved and input.resolved.model_id in dialect.tool_unsupported_model_ids else [
            *self.build_tools(input.tools), *(dialect.server_tools(input.extensions) if dialect.server_tools else []),
        ]
        request_extensions = wire_request_extensions(
            prepared,
            extra_omit=("context_cache_id", "context_cache_tag", "context_cache_reset_ttl"),
        )
        if dialect.cache_key_mode == "openai":
            request_extensions.setdefault("prompt_cache_key", stable_prompt_cache_key([
                input.context.system_text,
                ",".join(tool.name for tool in input.tools),
            ]))
        return OpenAIChatRequestPlan(
            params={
                **request_extensions,
                "model": input.resolved.model_id if input.resolved is not None else self._model,
                "messages": self._messages(input, dialect, prepared, replay_for_assistant),
                **({"tools": tools} if tools else {}),
            },
            prepared_extensions=prepared,
            dialect=dialect,
        )

    @staticmethod
    def _native_tool_calls(buffers: dict[int, dict]) -> list[dict]:
        return [{"id": call["id"], "type": "function", "function": {
            "name": call["name"], "arguments": call["args_buf"] or "{}",
        }} for call in buffers.values()]

    @staticmethod
    def _final_tool_calls(buffers: dict[int, dict]) -> list[ToolCall]:
        return [ToolCall(id=call["id"], name=call["name"], arguments=call["args_buf"] or "{}") for call in buffers.values()]

    def _replay_for_turn(
        self,
        dialect: Any,
        phase: str,
        model: str,
        content: str,
        tool_calls: list[ToolCall],
        reasoning_content: str,
        reasoning_details: Any,
        native_tool_calls: list[dict],
    ) -> dict | None:
        if dialect.replay_strategy == "generic_stream":
            return {"reasoning_content": reasoning_content} if phase == "stream" and (tool_calls or reasoning_content) else None
        if dialect.replay_strategy == "deepseek":
            if not reasoning_content.strip():
                return None
            result: dict[str, Any] = {
                "schema_version": 2, "provider": dialect.provider_id, "protocol": "openai-chat",
                "model": model, "reasoning_content": reasoning_content,
            }
        elif dialect.replay_strategy == "minimax":
            if not reasoning_content.strip() and reasoning_details is None:
                return None
            result = {"schema_version": 2, "provider": dialect.provider_id, "protocol": "openai-chat", "model": model}
            if reasoning_content.strip():
                result["reasoning_content"] = reasoning_content
            if reasoning_details is not None:
                result["reasoning_details"] = reasoning_details
        else:
            return None
        if native_tool_calls:
            result["tool_calls"] = native_tool_calls
        return result

    def normalize_usage(self, raw: Any):
        if raw is None:
            return None
        if not isinstance(raw, dict) and not hasattr(raw, "prompt_tokens"):
            raise ProtocolResponseError("openai-chat", "usage must be an object")
        _number(raw, "prompt_tokens")
        _number(raw, "completion_tokens")
        _number(raw, "total_tokens")
        details = _get(raw, "prompt_tokens_details")
        if details is not None:
            if not isinstance(details, dict) and not hasattr(details, "cached_tokens"):
                raise ProtocolResponseError("openai-chat", "usage.prompt_tokens_details must be an object")
            _number(details, "cached_tokens")
        completion_details = _get(raw, "completion_tokens_details")
        if completion_details is not None:
            if not isinstance(completion_details, dict) and not hasattr(completion_details, "reasoning_tokens"):
                raise ProtocolResponseError("openai-chat", "usage.completion_tokens_details must be an object")
            _number(completion_details, "reasoning_tokens")
        return normalize_usage(raw)

    def decode_complete(self, raw: Any, input: CanonicalAdapterInput, dialect: Any) -> OpenAIChatDecodeResult:
        choices = _get(raw, "choices") or []
        choice = _get(choices[0], "message") if choices else None
        content = _get(choice, "content") or ""
        native_calls = _get(choice, "tool_calls") or []
        tool_calls = []
        for call in native_calls:
            function = _get(call, "function")
            normalized = normalize_tool_call(_get(call, "id"), _get(function, "name"), _get(function, "arguments"))
            if normalized:
                tool_calls.append(normalized)
        usage = _get(raw, "usage")
        self.normalize_usage(usage)
        token_count = _number(usage, "completion_tokens") if usage is not None else None
        if token_count is None and usage is not None:
            token_count = _number(usage, "total_tokens")
        replay = self._replay_for_turn(
            dialect, "complete", input.resolved.model_id if input.resolved else self._model, content, tool_calls,
            _get(choice, "reasoning_content") or "", _get(choice, "reasoning_details"),
            [call if isinstance(call, dict) else {"id": _get(call, "id"), "function": {
                "name": _get(_get(call, "function"), "name"), "arguments": _get(_get(call, "function"), "arguments"),
            }} for call in native_calls],
        )
        return OpenAIChatDecodeResult(
            message=Message(role="assistant", content=content, tool_calls=tool_calls or None, token_count=token_count),
            replay=replay,
        )

    def create_stream_state(self, input: CanonicalAdapterInput, dialect: Any) -> OpenAIChatStreamState:
        return OpenAIChatStreamState(input=input, dialect=dialect)

    def _pending_tool_events(self, state: OpenAIChatStreamState) -> list[ToolCallEvent]:
        events = []
        for index, call in state.tool_call_buffers.items():
            if index in state.emitted_tool_call_indexes:
                continue
            try:
                args = json.loads(call["args_buf"] or "{}")
            except json.JSONDecodeError:
                args = {}
            state.emitted_tool_call_indexes.add(index)
            events.append(ToolCallEvent(id=call["id"], name=call["name"], arguments=args))
        return events

    def _stream_replay(self, state: OpenAIChatStreamState) -> dict | None:
        return self._replay_for_turn(
            state.dialect, "stream", state.input.resolved.model_id if state.input.resolved else self._model,
            state.accumulated_content, self._final_tool_calls(state.tool_call_buffers),
            state.accumulated_reasoning, state.accumulated_reasoning_details,
            self._native_tool_calls(state.tool_call_buffers),
        )

    def push_stream_chunk(self, chunk: Any, state: OpenAIChatStreamState) -> AdapterOutput:
        usage = _get(chunk, "usage")
        if usage is not None:
            self.normalize_usage(usage)
            state.total_tokens = _number(usage, "total_tokens") or 0
            state.input_tokens = _number(usage, "prompt_tokens") or 0
            state.output_tokens = _number(usage, "completion_tokens") or 0
            state.cache_read_tokens = openai_cached_prompt_tokens(usage)
            state.raw_usage = usage
            return AdapterOutput()
        choices = _get(chunk, "choices") or []
        choice = choices[0] if choices else None
        if choice is None:
            return AdapterOutput()
        finish_reason = _get(choice, "finish_reason")
        if finish_reason:
            state.finish_reason = finish_reason
        delta = _get(choice, "delta")
        if delta is None:
            return AdapterOutput()
        events = []
        reasoning = _get(delta, "reasoning_content")
        if reasoning:
            state.accumulated_reasoning += str(reasoning)
            if state.dialect.expose_reasoning(state.input.extensions):
                events.append(ThinkingDelta(delta=str(reasoning)))
        details = _get(delta, "reasoning_details")
        if details is not None:
            state.accumulated_reasoning_details = details
        content = _get(delta, "content")
        if content:
            if state.dialect.inline_thinking_tags:
                for part in state.extractor.feed(str(content)):
                    if part["type"] == "thinking":
                        state.accumulated_reasoning += part["content"]
                        events.append(ThinkingDelta(delta=part["content"]))
                    else:
                        state.accumulated_content += part["content"]
                        events.append(TextDelta(delta=part["content"]))
            else:
                state.accumulated_content += str(content)
                events.append(TextDelta(delta=str(content)))
        for call in _get(delta, "tool_calls") or []:
            index = int(_get(call, "index") or 0)
            state.tool_call_buffers.setdefault(index, {"id": _get(call, "id") or "", "name": "", "args_buf": ""})
            function = _get(call, "function")
            if _get(function, "name"):
                state.tool_call_buffers[index]["name"] += _get(function, "name")
            state.tool_call_buffers[index]["args_buf"] += _get(function, "arguments") or ""
        if finish_reason == "tool_calls":
            events.extend(self._pending_tool_events(state))
            return AdapterOutput(events=events, replay=self._stream_replay(state))
        return AdapterOutput(events=events)

    def finish_stream(self, state: OpenAIChatStreamState, final: Any = None) -> AdapterOutput:
        events = []
        if final is not None:
            pushed = self.push_stream_chunk(final, state)
            events.extend(pushed.events)
        if state.dialect.inline_thinking_tags:
            for part in state.extractor.flush():
                if part["type"] == "thinking":
                    state.accumulated_reasoning += part["content"]
                    events.append(ThinkingDelta(delta=part["content"]))
                else:
                    state.accumulated_content += part["content"]
                    events.append(TextDelta(delta=part["content"]))
        events.extend(self._pending_tool_events(state))
        if state.total_tokens:
            events.append(UsageEvent(
                total_tokens=state.total_tokens,
                input_tokens=state.input_tokens,
                output_tokens=state.output_tokens,
                cache_read_input_tokens=state.cache_read_tokens,
                stop_reason=canonicalize_stop_reason(state.finish_reason),
                raw_stop_reason=state.finish_reason,
                provider_usage=self.normalize_usage(state.raw_usage),
            ))
        return AdapterOutput(events=events, replay=self._stream_replay(state))
