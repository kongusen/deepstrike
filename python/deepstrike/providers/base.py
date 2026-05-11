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


@runtime_checkable
class LLMProvider(Protocol):
    async def complete(self, messages: list[Message], tools: list[ToolSchema]) -> Message: ...
    async def stream(self, messages: list[Message], tools: list[ToolSchema], extensions: dict | None = None) -> AsyncIterator[StreamEvent]: ...
