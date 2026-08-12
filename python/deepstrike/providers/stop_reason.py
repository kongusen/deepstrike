"""Canonical stop-reason vocabulary shared with the Node runtime (spc_013 §3.5).

Providers normalize raw protocol spellings to the kernel's closed vocabulary before emitting a
``UsageEvent``. The original raw value is preserved in ``UsageEvent.raw_stop_reason`` for
diagnostics but is never forwarded to the kernel.
"""
from __future__ import annotations

from typing import Literal

CanonicalStopReason = Literal[
    "end_turn",
    "tool_use",
    "max_tokens",
    "stop_sequence",
    "content_filter",
    "other",
]

_OPENAI_CHAT_MAPPING = {
    "stop": "end_turn",
    "length": "max_tokens",
    "tool_calls": "tool_use",
    "function_call": "tool_use",
    "content_filter": "content_filter",
}

_OPENAI_RESPONSES_MAPPING = {
    "max_output_tokens": "max_tokens",
    "content_filter": "content_filter",
}

_ANTHROPIC_MAPPING = {
    "end_turn": "end_turn",
    "tool_use": "tool_use",
    "max_tokens": "max_tokens",
    "stop_sequence": "stop_sequence",
    "content_filter": "content_filter",
}

_GEMINI_MAPPING = {
    "stop": "end_turn",
    "finish_reason_stop": "end_turn",
    "max_tokens": "max_tokens",
    "finish_reason_max_tokens": "max_tokens",
    "safety": "content_filter",
    "finish_reason_safety": "content_filter",
}

_OLLAMA_MAPPING = {
    "stop": "end_turn",
    "length": "max_tokens",
}


def canonicalize_stop_reason(raw: str | None) -> CanonicalStopReason | None:
    """Normalize any protocol-raw finish reason to the canonical vocabulary."""
    if not isinstance(raw, str) or not raw:
        return None
    normalized = raw.strip().lower()
    if not normalized:
        return None

    combined = {
        **_OPENAI_CHAT_MAPPING,
        **_OPENAI_RESPONSES_MAPPING,
        **_ANTHROPIC_MAPPING,
        **_GEMINI_MAPPING,
        **_OLLAMA_MAPPING,
    }
    return combined.get(normalized, "other")
