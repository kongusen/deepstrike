from deepstrike._kernel import ToolCall
from deepstrike.runtime.session_repair import (
  build_llm_completed_event,
  effective_provider_replay,
  normalize_llm_completed,
  repair_events_for_recovery,
  synthesize_provider_replay,
)
from deepstrike.runtime.session_log import SessionEntry


def test_synthesize_provider_replay_for_tool_turn():
  assert synthesize_provider_replay("checking", []) is None
  replay = synthesize_provider_replay("checking", [
    ToolCall(id="c1", name="ping", arguments="{}"),
  ])
  assert replay and replay.get("native_blocks")


def test_repair_events_fills_provider_replay():
  entries = [SessionEntry(seq=0, event={
    "kind": "llm_completed",
    "turn": 0,
    "content": "checking",
    "tool_calls": [{"id": "c1", "name": "ping", "arguments": "{}"}],
  })]
  repaired = repair_events_for_recovery(entries)
  assert repaired[0].event.get("provider_replay")


def test_build_llm_completed_always_has_tool_calls():
  event = build_llm_completed_event(turn=0, content="x", tool_calls=[])
  assert event["tool_calls"] == []


def test_effective_provider_replay_prefers_reasoning():
  replay = effective_provider_replay("x", [], {"reasoning_content": "trace"})
  assert replay == {"reasoning_content": "trace"}


def test_normalize_llm_completed_estimates_tokens():
  event = normalize_llm_completed({"kind": "llm_completed", "turn": 0, "content": "hello"})
  assert event["token_count"] >= 1
