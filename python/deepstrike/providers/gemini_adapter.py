"""Gemini protocol conversion with a complete request/stream/finalize lifecycle."""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.base import RenderedContext, UnsupportedModalityError
from deepstrike.providers.protocol_adapter import AdapterOutput, ProtocolResponseError
from deepstrike.providers.stop_reason import canonicalize_stop_reason
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike.providers.usage import ProviderUsage
from deepstrike.types.content import CanonicalAdapterInput, normalize_tool_result, project_tool_output_to_text


def _get(value: Any, name: str) -> Any:
    return value.get(name) if isinstance(value, dict) else getattr(value, name, None)


def _usage_number(raw: Any, field: str) -> int | None:
    value = _get(raw, field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ProtocolResponseError("gemini", f"usage.{field} must be a non-negative number")
    return int(value)


@dataclass(frozen=True)
class GeminiRequestPlan:
    contents: list[dict]
    config: dict | None


@dataclass
class GeminiStreamState:
    tool_calls: list[dict] = field(default_factory=list)
    last_usage: Any = None
    raw_stop_reason: str | None = None


class GeminiAdapter:
    protocol = "gemini"

    def __init__(self, model: str):
        self._model = model

    def build_contents(self, turns: list[Message]) -> list[dict]:
        contents: list[dict] = []
        for msg in turns:
            if msg.role == "tool":
                parts = []
                for part in getattr(msg, "content_parts", []):
                    if part.type != "tool_result":
                        continue
                    tool_name = part.call_id
                    for turn in reversed(turns):
                        matches = getattr(turn, "tool_calls", None) or []
                        match = next((call for call in matches if call.id == part.call_id), None)
                        if match is not None:
                            tool_name = match.name
                            break
                    parts.append({"function_response": {"name": tool_name, "response": {
                        "output": project_tool_output_to_text(normalize_tool_result(
                            part.call_id, part.output, part.is_error, getattr(part, "content_parts", None),
                        ).blocks),
                    }}})
                if parts:
                    contents.append({"role": "user", "parts": parts})
                continue

            parts: list[dict] = []
            for call in getattr(msg, "tool_calls", None) or []:
                try:
                    args = json.loads(call.arguments)
                except json.JSONDecodeError:
                    args = {}
                parts.append({"function_call": {"name": call.name, "args": args}})
            content_parts = getattr(msg, "content_parts", None) or []
            if content_parts:
                for part in content_parts:
                    if part.type == "text":
                        parts.append({"text": part.text})
                    elif part.type in {"image", "audio"}:
                        media_type = part.media_type or ("image/png" if part.type == "image" else "audio/wav")
                        if getattr(part, "data", None):
                            parts.append({"inline_data": {"mime_type": media_type, "data": part.data}})
                        elif getattr(part, "url", None):
                            parts.append({"file_data": {"mime_type": media_type, "file_uri": part.url}})
                        elif part.type == "audio":
                            raise UnsupportedModalityError("audio", "gemini")
                    elif part.type != "tool_result":
                        raise UnsupportedModalityError(getattr(part, "type", "unknown"), "gemini")
            elif msg.content:
                parts.append({"text": msg.content})
            if parts:
                contents.append({"role": "model" if msg.role == "assistant" else "user", "parts": parts})
        return contents

    @staticmethod
    def build_tools(tools: list[ToolSchema] | tuple[ToolSchema, ...]) -> list[dict] | None:
        if not tools:
            return None
        return [{"function_declarations": [{
            "name": tool.name,
            "description": tool.description,
            "parameters_json_schema": json.loads(tool.parameters),
        } for tool in tools]}]

    def build_config(self, system: str | None, tools: list[ToolSchema] | tuple[ToolSchema, ...], extensions: dict | None) -> dict | None:
        ext = extensions or {}
        config: dict = {}
        if system:
            config["system_instruction"] = system
        tool_defs = list(self.build_tools(tools) or [])
        if ext.get("google_search"):
            tool_defs.append({"google_search": ext["google_search"] if isinstance(ext["google_search"], dict) else {}})
        if tool_defs:
            config["tools"] = tool_defs
            config["automatic_function_calling"] = {"disable": True}
        for key in ("thinking_config", "response_mime_type", "response_schema", "cached_content"):
            if ext.get(key) is not None:
                config[key] = ext[key]
        return config or None

    def build_request(self, input: CanonicalAdapterInput) -> GeminiRequestPlan:
        context = input.context
        turns = [*context.turns, *([context.state_turn] if context.state_turn is not None else [])]
        return GeminiRequestPlan(
            contents=self.build_contents(turns),
            config=self.build_config(context.system_text or None, input.tools, input.extensions),
        )

    @staticmethod
    def _response_parts(response: Any) -> list:
        parts = _get(response, "parts")
        if parts:
            return list(parts)
        candidates = _get(response, "candidates") or []
        content = _get(candidates[0], "content") if candidates else None
        return list(_get(content, "parts") or [])

    @staticmethod
    def _function_call(part: Any) -> Any:
        return _get(part, "function_call")

    def decode_complete(self, raw: Any, input: CanonicalAdapterInput) -> Message:
        content = ""
        tool_calls = []
        for part in self._response_parts(raw):
            text = _get(part, "text")
            if text:
                content += text
                continue
            call = self._function_call(part)
            if call:
                name = str(_get(call, "name") or "")
                args = _get(call, "args") or {}
                if name:
                    from deepstrike.providers.base import normalize_tool_call
                    normalized = normalize_tool_call(name, name, args)
                    if normalized:
                        tool_calls.append(normalized)
        usage = _get(raw, "usage_metadata")
        total = _usage_number(usage, "total_token_count") if usage is not None else None
        return Message(role="assistant", content=content, token_count=total, tool_calls=tool_calls or None)

    def create_stream_state(self, input: CanonicalAdapterInput) -> GeminiStreamState:
        return GeminiStreamState()

    def push_stream_chunk(self, chunk: Any, state: GeminiStreamState) -> AdapterOutput:
        usage = _get(chunk, "usage_metadata")
        if usage is not None:
            state.last_usage = usage
        candidates = _get(chunk, "candidates") or []
        if candidates:
            reason = _get(candidates[0], "finish_reason")
            if isinstance(reason, str) and reason:
                state.raw_stop_reason = reason
        events = []
        for part in self._response_parts(chunk):
            text = _get(part, "text")
            if text:
                events.append(TextDelta(delta=text))
                continue
            call = self._function_call(part)
            if call:
                state.tool_calls.append({"name": str(_get(call, "name") or ""), "args": _get(call, "args") or {}})
        return AdapterOutput(events=events)

    def finish_stream(self, state: GeminiStreamState, final: Any = None) -> AdapterOutput:
        if final is not None:
            self.push_stream_chunk(final, state)
        events = [ToolCallEvent(id=f"call_{index + 1}", name=call["name"], arguments=call["args"])
                  for index, call in enumerate(state.tool_calls)]
        if state.last_usage is None:
            return AdapterOutput(events=events)
        usage = self.normalize_usage(state.last_usage)
        total = _usage_number(state.last_usage, "total_token_count") or 0
        if total:
            events.append(UsageEvent(
                total_tokens=total,
                input_tokens=usage.input_tokens if usage else 0,
                output_tokens=usage.output_tokens if usage else 0,
                cache_read_input_tokens=usage.cache_read_input_tokens if usage else 0,
                stop_reason=canonicalize_stop_reason(state.raw_stop_reason),
                raw_stop_reason=state.raw_stop_reason,
                provider_usage=usage,
            ))
        return AdapterOutput(events=events)

    def normalize_usage(self, raw: Any) -> ProviderUsage | None:
        if raw is None:
            return None
        input_tokens = _usage_number(raw, "prompt_token_count")
        output_tokens = _usage_number(raw, "candidates_token_count")
        cache_read = _usage_number(raw, "cached_content_token_count")
        _usage_number(raw, "total_token_count")
        if input_tokens is None and output_tokens is None and cache_read is None:
            return None
        return ProviderUsage(
            input_tokens=input_tokens or 0,
            output_tokens=output_tokens or 0,
            cache_read_input_tokens=cache_read or 0,
        )
