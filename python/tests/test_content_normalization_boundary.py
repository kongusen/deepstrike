"""P-02 canonical provider-input normalization contracts."""
from __future__ import annotations

import pytest

from deepstrike._kernel import ContentPartObj, Message, ToolSchema
from deepstrike.providers.base import RenderedContext, to_openai_message_params
from deepstrike.providers.openai_responses import OpenAIResponsesAdapter
from deepstrike.types.content import (
    ContentValidationError,
    RenderedMessage,
    StructuredToolResultPart,
    normalize_canonical_adapter_input,
)


def _tools() -> list[ToolSchema]:
    return [ToolSchema(name="lookup", description="Lookup", parameters="{}")]


def test_normalizer_returns_a_single_adapter_input_without_mutating_caller_fields() -> None:
    context = RenderedContext(turns=[Message(role="user", content="hello")])
    extensions = {"temperature": 0.2}

    canonical = normalize_canonical_adapter_input(context, _tools(), extensions=extensions)

    assert canonical.context is context
    assert len(canonical.tools) == 1
    assert canonical.tools[0].name == "lookup"
    assert canonical.extensions == {"temperature": 0.2}
    assert extensions == {"temperature": 0.2}


def test_normalizer_rejects_nested_tool_result_before_provider_serialization() -> None:
    context = RenderedContext(turns=[RenderedMessage(
        role="tool",
        content_parts=[StructuredToolResultPart(
            call_id="call-1",
            output="[tool_result]",
            content_parts=[{"type": "tool_result", "text": "nested"}],
        )],
    )])

    with pytest.raises(ContentValidationError, match="nested tool_result"):
        normalize_canonical_adapter_input(context, _tools())


@pytest.mark.parametrize("part", [
    ContentPartObj("image", media_type="image/png"),
    ContentPartObj("audio", media_type="audio/wav"),
])
def test_openai_serialization_rejects_media_without_a_source(part: ContentPartObj) -> None:
    context = RenderedContext(turns=[Message(role="user", content="", content_parts=[part])])

    with pytest.raises(ContentValidationError, match="source"):
        to_openai_message_params(context)


def test_normalizer_rejects_conflicting_tool_result_projection() -> None:
    context = RenderedContext(turns=[RenderedMessage(
        role="tool",
        content_parts=[StructuredToolResultPart(
            call_id="call-1",
            output="wrong projection",
            content_parts=[{"type": "text", "text": "canonical"}],
        )],
    )])

    with pytest.raises(ContentValidationError, match="projection conflict"):
        normalize_canonical_adapter_input(context, _tools())


def test_responses_adapter_uses_the_same_media_validation_boundary() -> None:
    context = RenderedContext(turns=[Message(
        role="user",
        content="",
        content_parts=[ContentPartObj("image", media_type="image/png")],
    )])

    with pytest.raises(ContentValidationError, match="source"):
        OpenAIResponsesAdapter().build_input(context)
