"""Normalized provider usage parsing.

Mirrors the Node ``ProviderUsage`` contract: a raw provider usage object is reduced to a small
set of cross-provider fields. Missing usage returns ``None``; malformed fields raise a protocol
``ProviderError`` rather than being silently coerced to zero.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from .provider_error import ProviderError


@dataclass(frozen=True)
class ProviderUsage:
    input_tokens: int
    output_tokens: int
    cache_read_input_tokens: int = 0
    cache_creation_input_tokens: int = 0
    reasoning_tokens: int | None = None


def _get(obj: Any, key: str) -> Any:
    if obj is None:
        return None
    if isinstance(obj, dict):
        return obj.get(key)
    return getattr(obj, key, None)


def _number(raw: Any, field: str) -> int | None:
    """Return a non-negative integer for a usage field, or None if absent.

    Raises ``ProviderError(kind="protocol")`` when the field is present but not a number.
    """
    if raw is None:
        return None
    value = _get(raw, field)
    if value is None:
        return None
    # Exclude bool (subclass of int) and other non-numeric types.
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ProviderError(
            provider="usage",
            kind="protocol",
            retryable=False,
            message=f"usage field {field!r} is not numeric: {type(value).__name__}",
        )
    if value < 0:
        raise ProviderError(
            provider="usage",
            kind="protocol",
            retryable=False,
            message=f"usage field {field!r} is negative: {value}",
        )
    return int(value)


def _first_number(raw: Any, fields: tuple[str, ...]) -> int | None:
    for field in fields:
        value = _number(raw, field)
        if value is not None:
            return value
    return None


def _nested_number(raw: Any, outer: str, inner: str) -> int | None:
    obj = _get(raw, outer)
    if obj is None:
        return None
    return _number(obj, inner)


def normalize_usage(raw_usage: Any) -> ProviderUsage | None:
    """Parse a raw provider usage object into a normalized ``ProviderUsage``.

    Returns ``None`` when the response contains no usable usage fields.
    """
    if raw_usage is None:
        return None

    input_tokens = _first_number(raw_usage, (
        "input_tokens",
        "prompt_tokens",
        "inputTokenCount",
    ))
    output_tokens = _first_number(raw_usage, (
        "output_tokens",
        "completion_tokens",
        "candidates_token_count",
        "generatedTokenCount",
    ))

    if input_tokens is None and output_tokens is None:
        return None

    cache_read = _first_number(raw_usage, (
        "cache_read_input_tokens",
        "cached_tokens",
        "prompt_cache_hit_tokens",
        "cached_content_token_count",
    ))
    if cache_read is None:
        cache_read = _nested_number(raw_usage, "prompt_tokens_details", "cached_tokens")
    if cache_read is None:
        cache_read = _nested_number(raw_usage, "input_tokens_details", "cached_tokens")

    cache_creation = _first_number(raw_usage, ("cache_creation_input_tokens",))
    if cache_creation is None:
        cache_creation = _nested_number(raw_usage, "prompt_tokens_details", "cache_creation_tokens")

    reasoning = _first_number(raw_usage, ("reasoning_tokens",))
    if reasoning is None:
        reasoning = _nested_number(raw_usage, "output_tokens_details", "reasoning_tokens")

    return ProviderUsage(
        input_tokens=input_tokens or 0,
        output_tokens=output_tokens or 0,
        cache_read_input_tokens=cache_read or 0,
        cache_creation_input_tokens=cache_creation or 0,
        reasoning_tokens=reasoning,
    )
