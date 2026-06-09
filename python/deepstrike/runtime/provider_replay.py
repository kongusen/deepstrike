from __future__ import annotations

from typing import Any, TypedDict

from deepstrike._kernel import ToolCall

class ProviderReplay(TypedDict, total=False):
    schema_version: int
    provider: str
    protocol: str
    model: str
    native_blocks: list[dict[str, Any]]
    reasoning_content: str
    reasoning_details: Any
    native_message: Any
    tool_calls: list[Any]


def _replay_protocol(replay: dict[str, Any]) -> str | None:
    """Infer the wire protocol a stored replay envelope belongs to.

    New envelopes carry an explicit ``protocol``; legacy envelopes are inferred
    from shape (Anthropic ``native_blocks`` vs OpenAI ``reasoning_content`` /
    ``reasoning_details``)."""
    if replay.get("protocol"):
        return replay["protocol"]
    if replay.get("native_blocks"):
        return "anthropic-messages"
    if replay.get("reasoning_content") is not None or replay.get("reasoning_details") is not None:
        return "openai-chat"
    return None


def is_replay_compatible_with_provider(replay: dict[str, Any], descriptor: Any) -> bool:
    """A stored replay may only be seeded into a provider speaking the same wire
    protocol; cross-protocol envelopes are skipped so the new provider
    re-serializes neutral context instead."""
    if descriptor is None:
        return True
    protocol = _replay_protocol(replay)
    if protocol is None:
        return True
    return protocol == getattr(descriptor, "protocol", None)


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
        if stored and not is_replay_compatible_with_provider(stored, descriptor):
            continue
        # Pass the message even with no persisted replay: a provider may
        # reconstruct a legacy replay (e.g. Anthropic native_blocks) from the
        # neutral transcript. Providers that cannot reconstruct simply no-op.
        seed(event.get("content", ""), tool_calls, stored or {})


def peek_provider_replay(provider: Any, content: str, tool_calls: list[ToolCall]) -> ProviderReplay | None:
    peek = getattr(provider, "peek_provider_replay", None)
    if not callable(peek):
        return None
    return peek(content, tool_calls)
