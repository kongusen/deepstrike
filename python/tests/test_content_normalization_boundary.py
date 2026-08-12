"""P-02 canonical provider-input normalization contracts."""
from __future__ import annotations

import pytest
from dataclasses import replace

from deepstrike._kernel import ContentPartObj, Message, ToolSchema
from deepstrike.providers.base import RenderedContext, to_openai_message_params
from deepstrike.providers.model_registry import ModelDescriptor, model_registry, resolve_effective_capabilities
from deepstrike.providers.openai_responses import OpenAIResponsesAdapter
from deepstrike.providers.runtime_registry import create_provider
from deepstrike.runtime.kernel_step import message_to_kernel
from deepstrike.runtime.archive import FileArchiveStore
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


def test_known_runtime_rejects_explicitly_unsupported_audio_before_serialization() -> None:
    model = ModelDescriptor(
        id="openai/text-only",
        provider_id="openai",
        kind="generation",
        intrinsic_input_modalities=("text",),
    )
    runtime = model_registry.resolve_provider_runtime(
        "openai",
        "text-only",
        endpoint_overrides=None,
    )
    runtime = replace(
        runtime,
        model=model,
        effective_capabilities=resolve_effective_capabilities(model, "openai.chat"),
    )
    context = RenderedContext(turns=[Message(
        role="user",
        content="",
        content_parts=[ContentPartObj("audio", data="YWJj", media_type="audio/wav")],
    )])

    with pytest.raises(ContentValidationError, match="audio is explicitly unsupported"):
        normalize_canonical_adapter_input(context, [], resolved=runtime)


def test_unknown_runtime_keeps_audio_fail_open_at_canonical_boundary() -> None:
    runtime = model_registry.resolve_provider_runtime("openai", "unregistered-model")
    context = RenderedContext(turns=[Message(
        role="user",
        content="",
        content_parts=[ContentPartObj("audio", data="YWJj", media_type="audio/wav")],
    )])

    canonical = normalize_canonical_adapter_input(context, [], resolved=runtime)

    assert canonical.resolved is runtime


def test_runtime_preflight_recursively_rejects_unsupported_tool_result_audio_source() -> None:
    runtime = model_registry.resolve_provider_runtime("openai", "unregistered-model")
    context = RenderedContext(turns=[RenderedMessage(
        role="tool",
        content_parts=[StructuredToolResultPart(
            call_id="call-1",
            output="[audio]",
            content_parts=[{
                "type": "audio",
                "source": {"kind": "url", "url": "https://example.test/input.wav"},
            }],
        )],
    )])

    with pytest.raises(ContentValidationError, match="audio url source is explicitly unsupported"):
        normalize_canonical_adapter_input(context, [], resolved=runtime)


def test_factory_attaches_runtime_and_provider_entry_uses_it_for_source_preflight() -> None:
    provider = create_provider("openai", api_key="key", model="unregistered-model")
    context = RenderedContext(turns=[Message(
        role="user",
        content="",
        content_parts=[ContentPartObj("audio", url="https://example.test/input.wav", media_type="audio/wav")],
    )])

    assert provider._resolved_runtime.provider_id == "openai"
    assert provider._resolved_runtime.model_id == provider._model
    with pytest.raises(ContentValidationError, match="audio url source is explicitly unsupported"):
        provider._build_messages(context)


def test_file_id_affinity_must_match_the_resolved_provider_endpoint() -> None:
    runtime = model_registry.resolve_provider_runtime(
        "openai",
        "gpt-5.5",
        endpoint_id="openai.responses",
    )
    context = RenderedContext(turns=[RenderedMessage(
        role="tool",
        content_parts=[StructuredToolResultPart(
            call_id="call-file",
            output="[file]",
            content_parts=[{
                "type": "file",
                "source": {
                    "kind": "fileId",
                    "id": "file_1",
                    "affinity": {"providerId": "openai", "endpointId": "openai.chat"},
                },
            }],
        )],
    )])

    with pytest.raises(ContentValidationError, match="belongs to openai/openai.chat"):
        normalize_canonical_adapter_input(context, [], resolved=runtime)


@pytest.mark.parametrize("affinity", [
    None,
    {"providerId": "openai", "endpointId": "openai.responses"},
])
def test_file_id_is_valid_at_its_affine_or_legacy_current_endpoint(affinity: dict | None) -> None:
    runtime = model_registry.resolve_provider_runtime(
        "openai",
        "gpt-5.5",
        endpoint_id="openai.responses",
    )
    source = {"kind": "fileId", "id": "file_1"}
    if affinity is not None:
        source["affinity"] = affinity
    context = RenderedContext(turns=[RenderedMessage(
        role="tool",
        content_parts=[StructuredToolResultPart(
            call_id="call-file",
            output="[file]",
            content_parts=[{"type": "file", "source": source}],
        )],
    )])

    canonical = normalize_canonical_adapter_input(context, [], resolved=runtime)

    assert canonical.resolved is runtime


def test_pyo3_file_carrier_enforces_affinity_serializes_to_responses_and_refuses_kernel_wire() -> None:
    runtime = model_registry.resolve_provider_runtime(
        "openai", "gpt-5.5", endpoint_id="openai.responses",
    )
    message = Message(role="user", content="", content_parts=[ContentPartObj(
        "file",
        file_id="file_1",
        provider_id="openai",
        endpoint_id="openai.responses",
    )])
    context = RenderedContext(turns=[message])

    canonical = normalize_canonical_adapter_input(context, [], resolved=runtime)

    assert OpenAIResponsesAdapter().build_input(context, resolved=runtime) == [{
        "role": "user", "content": [{"type": "input_file", "file_id": "file_1"}],
    }]
    assert canonical.resolved is runtime
    with pytest.raises(ValueError, match="fileId content is not supported by the kernel wire"):
        message_to_kernel(message)


@pytest.mark.asyncio
async def test_file_carrier_survives_python_archive_round_trip(tmp_path) -> None:
    archive = FileArchiveStore(tmp_path)
    message = Message(role="user", content="", content_parts=[ContentPartObj(
        "file",
        file_id="file_1",
        provider_id="openai",
        endpoint_id="openai.responses",
    )])

    archive_ref = await archive.write("session", 1, [message])
    restored = await archive.read(archive_ref)

    part = restored[0].content_parts[0]
    assert (part.type, part.file_id, part.provider_id, part.endpoint_id) == (
        "file", "file_1", "openai", "openai.responses",
    )
