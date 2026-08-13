from __future__ import annotations

from typing import Any, NotRequired, TypedDict

from deepstrike._kernel import ToolCall
from deepstrike.providers.replay import assistant_replay_key  # re-exported for runtime API stability

__all__ = [
    "ProviderReplay",
    "assistant_replay_key",
    "is_replay_compatible_with_provider",
    "seed_provider_replay_from_events",
    "peek_provider_replay",
    "assess_provider_replayability",
]

class ProviderReplay(TypedDict):
    protocol: str
    provider: NotRequired[str]
    model: NotRequired[str]
    native_blocks: NotRequired[list[dict[str, Any]]]
    reasoning_content: NotRequired[str]
    reasoning_details: NotRequired[Any]
    native_message: NotRequired[Any]
    tool_calls: NotRequired[list[Any]]


def is_replay_compatible_with_provider(replay: dict[str, Any], descriptor: Any) -> bool:
    """A stored replay may only be seeded into a provider speaking the same wire
    protocol; cross-protocol envelopes are skipped so the new provider
    re-serializes neutral context instead."""
    allowed = {"protocol", "provider", "model", "native_blocks", "reasoning_content", "reasoning_details", "native_message", "tool_calls"}
    unknown = set(replay) - allowed
    if unknown:
        raise ValueError(f"provider replay has unknown field {sorted(unknown)[0]}")
    protocol = replay.get("protocol")
    if not isinstance(protocol, str) or not protocol:
        raise ValueError("provider replay protocol is required")
    return descriptor is None or protocol == getattr(descriptor, "protocol", None)


def seed_provider_replay_from_events(provider: Any, events: list[Any]) -> None:
    seed = getattr(provider, "seed_provider_replay", None)
    if not callable(seed):
        return
    descriptor_fn = getattr(provider, "descriptor", None)
    descriptor = descriptor_fn() if callable(descriptor_fn) else None
    for entry in events:
        event = entry.event if hasattr(entry, "event") else entry
        if event.get("kind") != "llm_completed":
            continue
        tool_calls = event.get("tool_calls", [])
        stored = event.get("provider_replay")
        if not stored or not is_replay_compatible_with_provider(stored, descriptor):
            continue
        seed(event.get("content", ""), tool_calls, stored)


def peek_provider_replay(provider: Any, content: str, tool_calls: list[ToolCall]) -> ProviderReplay | None:
    peek = getattr(provider, "peek_provider_replay", None)
    if not callable(peek):
        return None
    return peek(content, tool_calls)


def assess_provider_replayability(provider: Any, context: Any, extensions: dict | None = None) -> dict:
    """Pre-flight query for fallback routing: would ``context`` validate against
    ``provider`` (with ``extensions``) before the request is sent? Seed any
    persisted replay first so the assessment reflects what the provider can
    actually replay. Providers without ``assess_replayability`` (no reasoning-
    replay requirement) are reported as ``ok``."""
    assess = getattr(provider, "assess_replayability", None)
    if not callable(assess):
        return {"ok": True, "offending_call_ids": []}
    return assess(context, extensions)
