import json
from pathlib import Path

import pytest

from deepstrike.runtime.durable_content import (
  DurableContentError,
  decode_durable_content,
  decode_durable_tool_result,
  durable_blocks_to_runtime,
  encode_durable_tool_result,
  runtime_blocks_to_durable,
)


def test_shared_fixture_round_trips():
  fixture = json.loads((Path(__file__).parents[2] / "tests/fixtures/durable-content/canonical-tool-result.json").read_text())
  assert encode_durable_tool_result(decode_durable_tool_result(fixture)) == fixture


def test_versioned_legacy_and_invalid_shapes_are_rejected():
  with pytest.raises(DurableContentError, match="unknown field"):
    decode_durable_content({"schema_version": 1, "blocks": []})
  with pytest.raises(DurableContentError, match="unknown field"):
    decode_durable_tool_result({"call_id": "call-1", "output": "old"})
  with pytest.raises(DurableContentError, match="unknown field"):
    decode_durable_content({"blocks": [{"type": "text", "text": "x", "extra": True}]})
  with pytest.raises(DurableContentError, match="nested"):
    decode_durable_content({"blocks": [{"type": "tool_result", "call_id": "nested"}]})
  with pytest.raises(DurableContentError, match="affinity"):
    decode_durable_content({"blocks": [{"type": "file", "source": {"kind": "file_id", "id": "f"}}]})


def test_file_affinity_and_object_ownership_are_retained():
  content = decode_durable_content({"blocks": [{"type": "file", "source": {"kind": "file_id", "id": "f", "affinity": {"provider_id": "p", "endpoint_id": "e"}}}]})
  assert content["blocks"][0]["source"]["affinity"]["endpoint_id"] == "e"
  with pytest.raises(DurableContentError, match="payload_ref"):
    decode_durable_content({"blocks": [{"type": "video", "source": {"kind": "object", "handle": "h", "owner": "host"}}]})
  with pytest.raises(DurableContentError, match="owner and payloadRef"):
    runtime_blocks_to_durable([{"type": "video", "source": {"kind": "object", "handle": "h"}}])


def test_provider_options_survive_runtime_replay_projection():
  blocks = durable_blocks_to_runtime([{"type": "image", "source": {"kind": "url", "url": "https://example.test/a"}, "provider_options": {"detail": "high"}}])
  assert blocks[0]["provider_options"] == {"detail": "high"}


def test_runtime_object_source_and_provider_options_round_trip_to_durable_abi():
  runtime = [{
    "type": "video",
    "source": {"kind": "object", "handle": "payload-9", "owner": "host", "payloadRef": "sha256:abc"},
    "media_type": "video/mp4",
    "provider_options": {"openai": {"detail": "high"}},
  }]

  durable = runtime_blocks_to_durable(runtime)

  assert durable == [{
    "type": "video",
    "source": {"kind": "object", "handle": "payload-9", "owner": "host", "payload_ref": "sha256:abc"},
    "media_type": "video/mp4",
    "provider_options": {"openai": {"detail": "high"}},
  }]
  assert durable_blocks_to_runtime(durable) == [{
    "type": "video",
    "source": {"kind": "object", "handle": "payload-9", "owner": "host", "payloadRef": "sha256:abc"},
    "media_type": "video/mp4",
    "provider_options": {"openai": {"detail": "high"}},
  }]


@pytest.mark.parametrize("value", [None, 0, 1, "false", []])
def test_is_error_must_be_boolean(value):
  with pytest.raises(DurableContentError, match="is_error must be a boolean"):
    decode_durable_tool_result({
      "call_id": "call-1",
      "is_error": value,
      "blocks": [],
    })


def test_is_error_is_required():
  with pytest.raises(DurableContentError, match="is_error must be a boolean"):
    decode_durable_tool_result({"call_id": "call-1", "blocks": []})
