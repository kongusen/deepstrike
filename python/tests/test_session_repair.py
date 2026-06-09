from deepstrike.runtime.session_repair import (
  build_llm_completed_event,
  normalize_llm_completed,
  repair_events_for_recovery,
)
from deepstrike.runtime.session_log import SessionEntry


def test_repair_does_not_synthesize_provider_replay():
  entries = [SessionEntry(seq=0, event={
    "kind": "llm_completed",
    "turn": 0,
    "content": "checking",
    "tool_calls": [{"id": "c1", "name": "ping", "arguments": "{}"}],
  })]
  repaired = repair_events_for_recovery(entries)
  # provider-neutral: no fabricated native_blocks
  assert "provider_replay" not in repaired[0].event


def test_repair_passes_stored_replay_through():
  stored = {"schema_version": 2, "provider": "deepseek", "protocol": "openai-chat", "reasoning_content": "trace"}
  entries = [SessionEntry(seq=0, event={
    "kind": "llm_completed",
    "turn": 0,
    "content": "x",
    "tool_calls": [],
    "provider_replay": stored,
  })]
  repaired = repair_events_for_recovery(entries)
  assert repaired[0].event["provider_replay"] == stored


def test_build_llm_completed_always_has_tool_calls():
  event = build_llm_completed_event(turn=0, content="x", tool_calls=[])
  assert event["tool_calls"] == []


def test_normalize_llm_completed_estimates_tokens():
  event = normalize_llm_completed({"kind": "llm_completed", "turn": 0, "content": "hello"})
  assert event["token_count"] >= 1
