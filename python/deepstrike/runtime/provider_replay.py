from __future__ import annotations

from typing import Any, TypedDict

from deepstrike._kernel import ToolCall
from deepstrike.providers.replay import assistant_replay_key

class ProviderReplay(TypedDict, total=False):
    native_blocks: list[dict[str, Any]]
    reasoning_content: str


def seed_provider_replay_from_events(provider: Any, events: list[Any]) -> None:
    seed = getattr(provider, "seed_provider_replay", None)
    if not callable(seed):
        return
    for entry in events:
        event = entry.event if hasattr(entry, "event") else entry
        if event.get("kind") != "llm_completed":
            continue
        replay = event.get("provider_replay")
        if not replay:
            continue
        seed(
            event.get("content", ""),
            event.get("tool_calls", []),
            replay,
        )


def peek_provider_replay(provider: Any, content: str, tool_calls: list[ToolCall]) -> ProviderReplay | None:
    peek = getattr(provider, "peek_provider_replay", None)
    if not callable(peek):
        return None
    return peek(content, tool_calls)
