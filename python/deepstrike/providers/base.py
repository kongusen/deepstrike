from __future__ import annotations
import asyncio
import json
import logging
import time
from typing import Any, AsyncIterator, Callable, Protocol, TypeVar, runtime_checkable
from dataclasses import dataclass, field
from deepstrike._kernel import Message, ToolCall, ToolSchema
from .stream import StreamEvent

logger = logging.getLogger(__name__)
T = TypeVar("T")


def parse_tool_arguments(value: Any) -> dict[str, Any]:
    """Normalize tool arguments to dict, handling str/dict/null."""
    if isinstance(value, dict):
        return value
    if not isinstance(value, str):
        return {}
    try:
        parsed = json.loads(value or "{}")
    except (TypeError, json.JSONDecodeError):
        return {}
    return parsed if isinstance(parsed, dict) else {}


def normalize_tool_call(call_id: Any, name: Any, arguments: Any) -> ToolCall | None:
    """Return normalized ToolCall or None if invalid."""
    normalized_name = str(name or "").strip()
    if not normalized_name:
        return None
    return ToolCall(
        id=str(call_id or ""),
        name=normalized_name,
        arguments=json.dumps(parse_tool_arguments(arguments)),
    )


@dataclass
class TokenUsage:
    input_tokens: int = 0
    output_tokens: int = 0

    @property
    def total_tokens(self) -> int:
        return self.input_tokens + self.output_tokens


@dataclass
class ProviderToolSpec:
    name: str
    description: str = ""
    parameters: dict = field(default_factory=dict)

    @classmethod
    def from_dict(cls, value: dict[str, Any]) -> "ProviderToolSpec":
        return cls(
            name=str(value["name"]),
            description=str(value.get("description", "")),
            parameters=value.get("parameters", {}),
        )

    def to_dict(self) -> dict[str, Any]:
        return {"name": self.name, "description": self.description, "parameters": self.parameters}


@dataclass
class RetryConfig:
    max_retries: int = 3
    base_delay: float = 1.0
    circuit_open_after: int = 5
    circuit_reset_after: float = 60.0


class CircuitBreaker:
    def __init__(self, config: RetryConfig):
        self._cfg = config
        self._failures = 0
        self._opened_at: float | None = None

    def is_open(self) -> bool:
        if self._opened_at is None:
            return False
        if time.monotonic() - self._opened_at >= self._cfg.circuit_reset_after:
            self._opened_at = None
            return False
        return True

    def record_success(self) -> None:
        self._failures = 0
        self._opened_at = None

    def record_failure(self) -> None:
        self._failures += 1
        if self._failures >= self._cfg.circuit_open_after:
            self._opened_at = time.monotonic()
            logger.warning("Circuit breaker opened after %d failures", self._failures)


def to_anthropic_content(msg: Message) -> str | list[dict]:
    """Convert Message to Anthropic API content format."""
    if not getattr(msg, "content_parts", None):
        return msg.content
    parts = []
    for p in msg.content_parts:
        if p.type == "text":
            parts.append({"type": "text", "text": p.text or ""})
        elif p.type == "image":
            if p.data:
                parts.append({"type": "image", "source": {"type": "base64", "media_type": p.media_type or "image/png", "data": p.data}})
            elif p.url:
                parts.append({"type": "image", "source": {"type": "url", "url": p.url}})
        elif p.type == "audio":
            parts.append({"type": "text", "text": f"[audio: {p.media_type}]"})
        elif p.type == "tool_result":
            parts.append({
                "type": "tool_result",
                "tool_use_id": p.call_id,
                "content": p.output,
                "is_error": p.is_error,
            })
    return parts or msg.content


def to_openai_content(msg: Message) -> str | list[dict]:
    """Convert Message to OpenAI API content format."""
    if not getattr(msg, "content_parts", None):
        return msg.content
    parts = []
    for p in msg.content_parts:
        if p.type == "text":
            parts.append({"type": "text", "text": p.text or ""})
        elif p.type == "image":
            if p.data:
                url = f"data:{p.media_type or 'image/png'};base64,{p.data}"
            else:
                url = p.url or ""
            image_url: dict = {"url": url}
            if p.detail:
                image_url["detail"] = p.detail
            parts.append({"type": "image_url", "image_url": image_url})
        elif p.type == "audio":
            fmt = (p.media_type or "audio/wav").split("/")[-1]
            parts.append({"type": "input_audio", "input_audio": {"data": p.data, "format": fmt}})
        elif p.type == "tool_result":
            parts.append({"type": "text", "text": p.output or ""})
    return parts or msg.content


def to_anthropic_messages(
    turns: list[Message],
    native_replay: Callable[[Message], list[dict] | None] | None = None,
) -> list[dict]:
    """Serialize provider-neutral turns into Anthropic-native messages."""
    result: list[dict] = []
    for msg in turns:
        if msg.role == "tool":
            parts = [
                {
                    "type": "tool_result",
                    "tool_use_id": p.call_id,
                    "content": p.output,
                    "is_error": p.is_error,
                }
                for p in (getattr(msg, "content_parts", None) or [])
                if p.type == "tool_result"
            ]
            if parts:
                result.append({"role": "user", "content": parts})
            continue

        if msg.role == "assistant" and getattr(msg, "tool_calls", None):
            replay = native_replay(msg) if native_replay else None
            if replay is not None:
                result.append({"role": "assistant", "content": replay})
                continue

            blocks: list[dict] = []
            if msg.content:
                blocks.append({"type": "text", "text": msg.content})
            for tc in msg.tool_calls:
                blocks.append({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": parse_tool_arguments(tc.arguments),
                })
            result.append({"role": "assistant", "content": blocks})
            continue

        result.append({"role": msg.role, "content": to_anthropic_content(msg)})
    return result


def to_openai_message_params(context: "RenderedContext") -> list[dict]:
    """Serialize provider-neutral context into OpenAI-compatible chat messages."""
    result: list[dict] = []
    if context.system_text:
        result.append({"role": "system", "content": context.system_text})

    for msg in context.turns:
        if msg.role == "tool":
            for p in (getattr(msg, "content_parts", None) or []):
                if p.type == "tool_result":
                    result.append({
                        "role": "tool",
                        "tool_call_id": p.call_id,
                        "content": p.output,
                    })
            continue

        next_msg: dict[str, Any] = {
            "role": msg.role,
            "content": to_openai_content(msg),
        }
        if msg.role == "assistant" and getattr(msg, "tool_calls", None):
            next_msg["tool_calls"] = [
                {
                    "id": tc.id,
                    "type": "function",
                    "function": {"name": tc.name, "arguments": tc.arguments},
                }
                for tc in msg.tool_calls
            ]
        result.append(next_msg)
    return result


@dataclass
class RenderedContext:
    system_text: str = ""
    turns: list[Message] = field(default_factory=list)


# Opaque per-run state owned by the provider (e.g. OpenAI Responses continuation).
ProviderRunState = dict[str, Any]


class RuntimePolicy:
    """Recommended runtime execution policy for a provider's model."""
    def __init__(self, *, max_turns: int | None = None, timeout_ms: int | None = None) -> None:
        self.max_turns = max_turns
        self.timeout_ms = timeout_ms


@runtime_checkable
class LLMProvider(Protocol):
    async def complete(self, context: RenderedContext, tools: list[ToolSchema], extensions: dict | None = None) -> Message: ...
    def stream(
        self,
        context: RenderedContext,
        tools: list[ToolSchema],
        extensions: dict | None = None,
        state: ProviderRunState | None = None,
    ) -> AsyncIterator[StreamEvent]: ...
    def runtime_policy(self) -> RuntimePolicy: ...
