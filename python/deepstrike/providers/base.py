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


class UnsupportedModalityError(ValueError):
    """Raised when a ContentPart modality cannot be mapped to a provider wire format."""

    def __init__(self, modality: str, provider: str):
        self.modality = modality
        self.provider = provider
        super().__init__(f"UnsupportedModality: {modality} is not supported by {provider}")

T = TypeVar("T")

# Internal control flags that steer DeepStrike's own serialization/validation
# and must never be forwarded to any provider's wire request.
INTERNAL_EXTENSION_KEYS = frozenset({"degrade_missing_reasoning_replay"})

# Keys the chat-completions transport sets itself; never echo them from extensions.
_WIRE_RESERVED_KEYS = frozenset({"model", "messages", "tools", "stream", "stream_options"})


def wire_request_extensions(extensions: dict | None, *, extra_omit: tuple[str, ...] = ()) -> dict:
    """Filter caller extensions down to what is safe to send on the wire,
    dropping transport-reserved keys and DeepStrike-internal control flags."""
    blocked = _WIRE_RESERVED_KEYS | INTERNAL_EXTENSION_KEYS | frozenset(extra_omit)
    return {k: v for k, v in (extensions or {}).items() if k not in blocked}


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
            raise UnsupportedModalityError("audio", "anthropic")
        elif p.type == "tool_result":
            parts.append({
                "type": "tool_result",
                "tool_use_id": p.call_id,
                "content": p.output,
                "is_error": p.is_error,
            })
    return parts or msg.content


def _openai_audio_format(media_type: str | None) -> str:
    """Map an audio MIME type to OpenAI's ``input_audio.format`` (accepts "mp3" | "wav").

    ``audio/mpeg`` must become "mp3", not the raw "mpeg" subtype.
    """
    sub = (media_type or "audio/wav").split("/")[-1].lower()
    if sub in ("mpeg", "mp3"):
        return "mp3"
    if sub in ("wav", "wave", "x-wav"):
        return "wav"
    return sub


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
            parts.append(
                {"type": "input_audio", "input_audio": {"data": p.data, "format": _openai_audio_format(p.media_type)}}
            )
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


def turns_with_state_appended(context: "RenderedContext") -> list:
    """History turns with the volatile State turn appended as the latest turn, for
    providers that render it inline (OpenAI-family, Gemini, Ollama). Appending
    keeps the history a byte-stable prefix so their automatic prefix caches hit
    across turns. Anthropic appends it after the cache breakpoint instead. When
    state_turn is absent (un-rebuilt binding) the State turn is still inside turns,
    so this returns turns as-is."""
    state_turn = getattr(context, "state_turn", None)
    return [*context.turns, state_turn] if state_turn is not None else list(context.turns)


def to_openai_message_params(context: "RenderedContext") -> list[dict]:
    """Serialize provider-neutral context into OpenAI-compatible chat messages."""
    result: list[dict] = []
    if context.system_text:
        result.append({"role": "system", "content": context.system_text})

    # The volatile State turn is appended as the latest turn so the history stays
    # a stable prefix that OpenAI's automatic cache can hit. Absent on un-rebuilt
    # bindings, where the state is already inside turns.
    for msg in turns_with_state_appended(context):
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


def stable_prompt_cache_key(parts: list[str]) -> str:
    """Deterministic short key for OpenAI's ``prompt_cache_key`` — groups requests
    that share a cacheable prefix (same system prompt + tool set) onto the same
    cache routing, improving automatic prefix-cache hit rates with no caller input.
    FNV-1a over the parts; stable across processes, no hashlib dependency."""
    h = 0x811C9DC5
    joined = " ".join(parts)
    for ch in joined:
        h ^= ord(ch) & 0xFF
        h = (h * 0x01000193) & 0xFFFFFFFF
    return f"ds-{h:08x}"


def openai_cached_prompt_tokens(usage: Any) -> int:
    """Cached-prompt-token count from an OpenAI-compatible usage object. Covers
    the standard ``prompt_tokens_details.cached_tokens`` (OpenAI, Qwen, MiniMax,
    GLM, Kimi) and DeepSeek's ``prompt_cache_hit_tokens``. These caches bill reads
    only, so there is no separate cache-creation count. The figure is a subset of
    ``prompt_tokens`` (the full prompt), surfaced for cost visibility."""
    if usage is None:
        return 0

    def _get(obj: Any, key: str) -> Any:
        if isinstance(obj, dict):
            return obj.get(key)
        return getattr(obj, key, None)

    details = _get(usage, "prompt_tokens_details")
    standard = _get(details, "cached_tokens") if details is not None else None
    deepseek = _get(usage, "prompt_cache_hit_tokens")
    return max(
        int(standard) if isinstance(standard, (int, float)) else 0,
        int(deepseek) if isinstance(deepseek, (int, float)) else 0,
    )


def cache_hit_rate(usage: Any) -> float:
    """Prompt-cache hit rate for one usage record: the fraction of the full prompt
    served from cache this request (``cache_read_input_tokens / input_tokens``,
    clamped to [0, 1]). Returns 0.0 when the prompt size is unknown. This is the
    headline metric for the prefix-cache work (P0-A) — across a long, append-only
    session it should climb and stay high; a sustained drop means the cacheable
    prefix is drifting. Accepts a ``UsageEvent``, a dict, or any object exposing
    those attributes (mirrors the Node ``cacheHitRate`` for SDK parity)."""

    def _get(obj: Any, key: str) -> Any:
        if isinstance(obj, dict):
            return obj.get(key)
        return getattr(obj, key, None)

    input_tokens = _get(usage, "input_tokens")
    if not isinstance(input_tokens, (int, float)) or input_tokens <= 0:
        return 0.0
    read = _get(usage, "cache_read_input_tokens")
    read = int(read) if isinstance(read, (int, float)) else 0
    return min(1.0, max(0.0, read / input_tokens))


@dataclass
class ContextBudgetOverflow:
    kind: str
    required_tokens: int
    max_tokens: int


@dataclass
class RenderedContext:
    system_text: str = ""
    turns: list[Message] = field(default_factory=list)
    # Identity partition (Anthropic system[0] with cache_control). Empty when the
    # kernel did not partition the system prompt.
    system_stable: str = ""
    # Knowledge partition (Anthropic system[1] with cache_control).
    system_knowledge: str = ""
    # Volatile State turn (task_state + signals), rendered after the cacheable
    # history. None when produced by an older binding — then the State turn is
    # still inside turns[0] and providers render turns as-is.
    state_turn: "Message | None" = None
    # P1-E: count of leading turns forming the frozen prefix (byte-stable until the
    # next compaction). The Anthropic provider pins a deep cache breakpoint here and
    # rolls the other at the tail; None ⇒ rolling-pair fallback.
    frozen_prefix_len: "int | None" = None
    # Fail-closed evidence from the kernel renderer. Provider effects are never emitted with this
    # set; direct projections expose it so hosts cannot mistake an invalid context for a sendable one.
    budget_overflow: "ContextBudgetOverflow | None" = None


# Opaque per-run state owned by the provider (e.g. OpenAI Responses continuation).
ProviderRunState = dict[str, Any]


class RuntimePolicy:
    """Recommended runtime execution policy for a provider's model."""
    def __init__(self, *, max_turns: int | None = None, timeout_ms: int | None = None) -> None:
        self.max_turns = max_turns
        self.timeout_ms = timeout_ms


# Wire protocols a provider can speak. Mirrors the Node/WASM ProviderProtocol union.
ProviderProtocol = str  # "anthropic-messages" | "openai-chat" | "openai-responses" | "gemini"


@dataclass
class ProviderDescriptor:
    """Stable identity advertised by a provider so the recovery layer can decide
    whether a stored replay envelope may be seeded into it."""
    provider: str
    protocol: ProviderProtocol
    model: str
    reasoning: dict[str, Any] = field(default_factory=dict)
    tool_calls: dict[str, Any] = field(default_factory=dict)


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


class ThinkingTagStreamExtractor:
    def __init__(self) -> None:
        self.buffer = ""
        self.in_thinking = False

    def feed(self, chunk: str):
        self.buffer += chunk
        while True:
            if not self.in_thinking:
                think_idx = self.buffer.find("<think>")
                if think_idx != -1:
                    text_before = self.buffer[:think_idx]
                    if text_before:
                        yield {"type": "text", "content": text_before}
                    self.in_thinking = True
                    self.buffer = self.buffer[think_idx + 7:]
                    continue

                possible_tag_start = self.buffer.rfind("<")
                if possible_tag_start != -1 and "<think>".startswith(self.buffer[possible_tag_start:]):
                    to_emit = self.buffer[:possible_tag_start]
                    if to_emit:
                        yield {"type": "text", "content": to_emit}
                    self.buffer = self.buffer[possible_tag_start:]
                    break
                else:
                    if self.buffer:
                        yield {"type": "text", "content": self.buffer}
                        self.buffer = ""
                    break
            else:
                end_think_idx = self.buffer.find("</think>")
                if end_think_idx != -1:
                    thinking_content = self.buffer[:end_think_idx]
                    if thinking_content:
                        yield {"type": "thinking", "content": thinking_content}
                    self.in_thinking = False
                    self.buffer = self.buffer[end_think_idx + 8:]
                    continue

                possible_end_start = self.buffer.rfind("<")
                if possible_end_start != -1 and "</think>".startswith(self.buffer[possible_end_start:]):
                    to_emit = self.buffer[:possible_end_start]
                    if to_emit:
                        yield {"type": "thinking", "content": to_emit}
                    self.buffer = self.buffer[possible_end_start:]
                    break
                else:
                    if self.buffer:
                        yield {"type": "thinking", "content": self.buffer}
                        self.buffer = ""
                    break

    def flush(self):
        if self.buffer:
            yield {"type": "thinking" if self.in_thinking else "text", "content": self.buffer}
            self.buffer = ""
