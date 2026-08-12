"""Ollama request conversion plus NDJSON stream lifecycle."""
from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Any

from deepstrike._kernel import Message, ToolSchema
from deepstrike.providers.base import UnsupportedModalityError, normalize_tool_call
from deepstrike.providers.protocol_adapter import AdapterOutput, ProtocolResponseError
from deepstrike.providers.stop_reason import canonicalize_stop_reason
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike.providers.usage import ProviderUsage
from deepstrike.types.content import CanonicalAdapterInput, normalize_tool_result, project_tool_output_to_text


def _number(raw: dict, field: str) -> int | None:
    value = raw.get(field)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value < 0:
        raise ProtocolResponseError("ollama-chat", f"{field} must be a non-negative number")
    return int(value)


class OllamaNdjsonDecoder:
    def __init__(self) -> None:
        self._buffer = ""

    def push(self, text: str) -> list[dict]:
        self._buffer += text
        lines = self._buffer.split("\n")
        self._buffer = lines.pop()
        return self._parse(lines)

    def finish(self, text: str = "") -> list[dict]:
        self._buffer += text
        tail, self._buffer = self._buffer, ""
        return self._parse([tail] if tail else [])

    @staticmethod
    def _parse(lines: list[str]) -> list[dict]:
        chunks = []
        for line in lines:
            if not line.strip():
                continue
            try:
                chunk = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(chunk, dict):
                chunks.append(chunk)
        return chunks


@dataclass
class OllamaStreamState:
    pending_tool_calls: dict[str, dict] = field(default_factory=dict)
    final_chunk: dict | None = None


class OllamaAdapter:
    protocol = "ollama-chat"

    def __init__(self, model: str = "llama3") -> None:
        self._model = model

    def create_ndjson_decoder(self) -> OllamaNdjsonDecoder:
        return OllamaNdjsonDecoder()

    def build_request(self, input: CanonicalAdapterInput) -> dict:
        context = input.context
        messages: list[dict] = []
        if context.system_text:
            messages.append({"role": "system", "content": context.system_text})
        turns = [*context.turns, *([context.state_turn] if context.state_turn is not None else [])]
        for message in turns:
            entry: dict = {"role": message.role, "content": message.content}
            parts = getattr(message, "content_parts", None) or []
            if any(part.type == "audio" for part in parts):
                raise UnsupportedModalityError("audio", "ollama")
            images = [part.data for part in parts if part.type == "image" and part.data]
            tool_results = [part for part in parts if part.type == "tool_result"]
            if tool_results:
                part = tool_results[0]
                entry["content"] = project_tool_output_to_text(normalize_tool_result(
                    part.call_id, part.output, part.is_error, getattr(part, "content_parts", None),
                ).blocks)
            if images:
                entry["images"] = images
            messages.append(entry)
        request = {
            **{key: value for key, value in input.extensions.items() if key not in {"model", "messages", "tools", "stream"}},
            "model": input.resolved.model_id if input.resolved is not None else self._model_from_context(input) or self._model,
            "messages": messages,
        }
        if input.tools:
            request["tools"] = [{"type": "function", "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": json.loads(tool.parameters),
            }} for tool in input.tools]
        return request

    @staticmethod
    def _model_from_context(input: CanonicalAdapterInput) -> str:
        return str(input.extensions.get("model") or "")

    def decode_complete(self, raw: dict, input: CanonicalAdapterInput) -> Message:
        message = raw.get("message") or {}
        tool_calls = []
        for call in message.get("tool_calls") or []:
            function = call.get("function") or {}
            normalized = normalize_tool_call(call.get("id", ""), function.get("name", ""), function.get("arguments", {}))
            if normalized:
                tool_calls.append(normalized)
        return Message(role="assistant", content=message.get("content") or "", token_count=0, tool_calls=tool_calls or None)

    def create_stream_state(self, input: CanonicalAdapterInput) -> OllamaStreamState:
        return OllamaStreamState()

    def push_stream_chunk(self, chunk: dict, state: OllamaStreamState) -> AdapterOutput:
        events = []
        message = chunk.get("message") or {}
        if message.get("content"):
            events.append(TextDelta(delta=message["content"]))
        for call in message.get("tool_calls") or []:
            function = call.get("function") or {}
            normalized = normalize_tool_call("", function.get("name", ""), function.get("arguments", {}))
            if normalized is None:
                continue
            key = f"{normalized.name}:{normalized.arguments}"
            if key not in state.pending_tool_calls:
                state.pending_tool_calls[key] = {
                    "id": f"call_{len(state.pending_tool_calls) + 1}",
                    "name": normalized.name,
                    "arguments": json.loads(normalized.arguments),
                }
        if chunk.get("done"):
            state.final_chunk = chunk
        return AdapterOutput(events=events)

    def finish_stream(self, state: OllamaStreamState, final: dict | None = None) -> AdapterOutput:
        terminal = final or state.final_chunk
        events = [ToolCallEvent(**call) for call in state.pending_tool_calls.values()]
        usage = self.normalize_usage(terminal)
        if usage is not None:
            raw_stop_reason = terminal.get("done_reason") if terminal else None
            events.append(UsageEvent(
                total_tokens=usage.input_tokens + usage.output_tokens,
                input_tokens=usage.input_tokens,
                output_tokens=usage.output_tokens,
                stop_reason=canonicalize_stop_reason(raw_stop_reason),
                raw_stop_reason=raw_stop_reason,
                provider_usage=usage,
            ))
        return AdapterOutput(events=events)

    def normalize_usage(self, raw: Any) -> ProviderUsage | None:
        if raw is None:
            return None
        if not isinstance(raw, dict):
            raise ProtocolResponseError("ollama-chat", "usage source must be an object")
        input_tokens = _number(raw, "prompt_eval_count")
        output_tokens = _number(raw, "eval_count")
        if input_tokens is None and output_tokens is None:
            return None
        return ProviderUsage(input_tokens=input_tokens or 0, output_tokens=output_tokens or 0)
