import pytest

from deepstrike.runtime.durable_content import DurableContentError
from deepstrike.runtime.durable_content import runtime_blocks_to_durable
from deepstrike.runtime.runner import _replay_messages
from deepstrike.runtime.session_log import SessionEntry


def test_replay_restores_additive_durable_blocks():
  blocks = runtime_blocks_to_durable([
    {"type": "text", "text": "first"},
    {"type": "image", "source": {"kind": "base64", "data": "aW1hZ2U="}, "media_type": "image/png"},
  ])
  messages = _replay_messages([SessionEntry(seq=0, event={
    "kind": "tool_completed", "turn": 1,
    "results": [{"call_id": "call-1", "output": "first\n[image]", "is_error": False, "content": {"blocks": blocks}}],
  })])
  part = messages[0].content_parts[0]
  assert part.call_id == "call-1"
  assert part.content_parts == [
    {"type": "text", "text": "first"},
    {"type": "image", "source": {"kind": "base64", "data": "aW1hZ2U="}, "media_type": "image/png"},
  ]


def test_replay_rejects_text_only_result_without_canonical_blocks():
  with pytest.raises(DurableContentError, match="no canonical content blocks"):
    _replay_messages([SessionEntry(seq=0, event={
      "kind": "tool_completed", "turn": 1,
      "results": [{"call_id": "removed", "output": "old", "is_error": False}],
    })])


def test_replay_rejects_malformed_durable_content_before_projection():
  with pytest.raises(DurableContentError, match="unknown field"):
    _replay_messages([SessionEntry(seq=0, event={
      "kind": "tool_completed", "turn": 1,
      "results": [{
        "call_id": "call-1", "output": "old", "is_error": False,
        "content": {"blocks": [{"type": "text", "text": "old", "extra": True}]},
      }],
    })])


def test_replay_rejects_non_boolean_is_error_before_consuming_result():
  with pytest.raises(DurableContentError, match="is_error must be a boolean"):
    _replay_messages([SessionEntry(seq=0, event={
      "kind": "tool_completed", "turn": 1,
      "results": [{
        "call_id": "removed", "output": "old", "is_error": "false",
        "content": {"blocks": [{"type": "text", "text": "old"}]},
      }],
    })])


def test_replay_rejects_missing_is_error_before_projection():
  with pytest.raises(DurableContentError, match="is_error must be a boolean"):
    _replay_messages([SessionEntry(seq=0, event={
      "kind": "tool_completed", "turn": 1,
      "results": [{
        "call_id": "call-1", "output": "old",
        "content": {"blocks": [{"type": "text", "text": "old"}]},
      }],
    })])
