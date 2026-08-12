from __future__ import annotations

import pytest

from deepstrike.providers.base import RenderedContext
from deepstrike.providers.model_registry import model_registry
from deepstrike.types.content import (
    RenderedMessage,
    StructuredToolResultPart,
    normalize_canonical_adapter_input,
)
from deepstrike.types.content_policy import ContentPolicyError, content_disposition_for


def test_policy_declares_native_bridge_and_unsupported_by_protocol_and_placement() -> None:
  assert content_disposition_for("anthropic-messages", "image", "tool_result") == "native"
  assert content_disposition_for("openai-responses", "image", "tool_result") == "native"
  assert content_disposition_for("openai-chat", "file", "tool_result") == "bridge"
  assert content_disposition_for("gemini", "audio", "tool_result") == "bridge"
  assert content_disposition_for("openai-responses", "file", "message") == "native"
  assert content_disposition_for("anthropic-messages", "video", "message") == "unsupported"
  assert content_disposition_for("ollama-chat", "file", "message") == "unsupported"


def test_policy_refuses_unsupported_video_before_a_serializer_can_flatten_it() -> None:
  runtime = model_registry.resolve_provider_runtime("anthropic", "claude-sonnet-4-6")
  context = RenderedContext(turns=[RenderedMessage(
    role="tool",
    content="[video]",
    content_parts=[StructuredToolResultPart(
      call_id="call-video",
      output="[video]",
      content_parts=[{
        "type": "video",
        "source": {"kind": "url", "url": "https://example.test/clip.mp4"},
        "media_type": "video/mp4",
      }],
    )],
  )])

  with pytest.raises(ContentPolicyError, match="Unsupported content policy: video"):
    normalize_canonical_adapter_input(context, [], resolved=runtime)
