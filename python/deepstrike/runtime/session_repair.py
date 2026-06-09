from __future__ import annotations

from typing import Any

from deepstrike._kernel import ToolCall
from deepstrike.runtime.replay_sanitize import sanitize_replay_text


def _estimate_token_count(text: str) -> int:
  return max(1, len(text) // 4)


def normalize_llm_completed(event: dict[str, Any], max_bytes: int | None = None) -> dict[str, Any]:
  """Normalize a persisted llm_completed event for recovery.

  Content is sanitized and token_count backfilled, but the stored
  ``provider_replay`` envelope is passed through verbatim. This layer is
  provider-neutral and never synthesizes protocol-specific replay shapes
  (e.g. Anthropic ``native_blocks``); legacy reconstruction for a given
  protocol is the target provider's ``seed_provider_replay`` responsibility.
  """
  content = sanitize_replay_text(event.get("content", ""), max_bytes)
  tool_calls = list(event.get("tool_calls") or [])
  provider_replay = event.get("provider_replay")
  out: dict[str, Any] = {
    "kind": "llm_completed",
    "turn": event["turn"],
    "content": content,
    "tool_calls": tool_calls,
    "token_count": event.get("token_count") or _estimate_token_count(content),
  }
  if provider_replay:
    out["provider_replay"] = provider_replay
  return out


def repair_events_for_recovery(events: list[Any], max_bytes: int | None = None) -> list[Any]:
  from deepstrike.runtime.session_log import SessionEntry

  repaired: list[Any] = []
  for entry in events:
    event = entry.event if hasattr(entry, "event") else entry
    if event.get("kind") != "llm_completed":
      repaired.append(entry)
      continue
    normalized = normalize_llm_completed(event, max_bytes)
    if hasattr(entry, "seq"):
      repaired.append(SessionEntry(seq=entry.seq, event=normalized))
    else:
      repaired.append(normalized)
  return repaired


def build_llm_completed_event(
  *,
  turn: int,
  content: str,
  tool_calls: list[ToolCall],
  token_count: int | None = None,
  provider_replay: dict[str, Any] | None = None,
) -> dict[str, Any]:
  return normalize_llm_completed({
    "kind": "llm_completed",
    "turn": turn,
    "content": content,
    "tool_calls": tool_calls,
    "token_count": token_count,
    "provider_replay": provider_replay,
  })


def build_run_terminal_event(*, reason: str, turns_used: int, total_tokens: int) -> dict[str, Any]:
  return {
    "kind": "run_terminal",
    "reason": reason,
    "turns_used": max(0, turns_used),
    "total_tokens": max(0, total_tokens),
  }
