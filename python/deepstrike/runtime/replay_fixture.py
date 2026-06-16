"""Fixture helpers for ReplayProvider.

Python port of node/src/runtime/replay-fixture.ts. Walks `llm_completed` events from a
session log and returns the ordered list of assistant Messages.

Accepts both wire shapes the SDK uses interchangeably:
- camelCase in-memory: `toolCalls`, `tokenCount`
- snake_case on-disk:  `tool_calls`, `token_count`
"""
from __future__ import annotations

from typing import Any, Iterable

from deepstrike._kernel import Message  # type: ignore


def extract_recorded_messages(events: Iterable[Any]) -> list[Message]:
    """Walk session events (either wrapped `{seq, event}` or bare events) into Message[]."""
    import json

    out: list[Message] = []
    for entry in events:
        event = entry.get("event") if isinstance(entry, dict) and "event" in entry else entry
        kind = event.get("kind") if isinstance(event, dict) else getattr(event, "kind", None)
        if kind != "llm_completed":
            continue

        def _g(k: str) -> Any:
            if isinstance(event, dict):
                return event.get(k)
            return getattr(event, k, None)

        content = _g("content") or ""
        # camelCase first, snake_case fallback
        raw_tc = _g("toolCalls") or _g("tool_calls") or []
        normalized_tc: list[dict] = []
        for tc in raw_tc:
            name = tc.get("name") if isinstance(tc, dict) else getattr(tc, "name", "")
            tc_id = tc.get("id") if isinstance(tc, dict) else getattr(tc, "id", "")
            args = tc.get("arguments") if isinstance(tc, dict) else getattr(tc, "arguments", "")
            if not isinstance(args, str):
                args = json.dumps(args)
            normalized_tc.append({"id": tc_id, "name": name, "arguments": args})
        token_count = _g("tokenCount") or _g("token_count")

        msg: dict[str, Any] = {"role": "assistant", "content": content}
        if normalized_tc:
            msg["toolCalls"] = normalized_tc
        if token_count is not None:
            msg["tokenCount"] = token_count
        out.append(msg)  # type: ignore[arg-type]
    return out
