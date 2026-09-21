"""P-05 Anthropic Messages ProtocolAdapter lifecycle contracts."""
from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from deepstrike._kernel import ModelMessage, ToolSchema
from deepstrike.providers.anthropic_adapter import (
    ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER,
    AnthropicMessagesAdapter,
)
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext, RetryConfig
from deepstrike.providers.model_registry import model_registry
from deepstrike.providers.stream import ThinkingDelta, ToolCallEvent, UsageEvent
from deepstrike.providers.protocol_adapter import ProtocolResponseError
from deepstrike.providers.provider_error import classify_provider_error
from deepstrike.types.content import normalize_canonical_adapter_input


def _input(extensions: dict | None = None):
    return normalize_canonical_adapter_input(
        RenderedContext(
            system_text="stable\n\nknowledge",
            system_stable="stable",
            system_knowledge="knowledge",
            turns=[ModelMessage(role="user", content="hi")],
        ),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        extensions=extensions,
        resolved=model_registry.resolve_provider_runtime("anthropic", "claude-sonnet-4-6"),
    )


def test_adapter_builds_stable_and_beta_request_plans() -> None:
    adapter = AnthropicMessagesAdapter()
    stable = adapter.build_request(_input())
    beta = adapter.build_request(_input({"betas": ["code-execution-2025-08-25"]}))

    assert stable.transport == "stable"
    assert beta.transport == "beta"
    assert beta.params["betas"] == ["code-execution-2025-08-25"]
    assert beta.params["messages"] == stable.params["messages"]
    assert stable.params["system"] == [
        {"type": "text", "text": "stable", "cache_control": {"type": "ephemeral"}},
        {"type": "text", "text": "knowledge", "cache_control": {"type": "ephemeral"}},
    ]


def test_adapter_decodes_complete_response_and_returns_native_replay() -> None:
    message, replay = AnthropicMessagesAdapter().decode_complete(SimpleNamespace(
        content=[
            SimpleNamespace(type="thinking", thinking="plan", signature="sig"),
            SimpleNamespace(type="text", text="done"),
            SimpleNamespace(type="tool_use", id="call_2", name="lookup", input={"q": "y"}),
        ],
        usage=SimpleNamespace(input_tokens=10, output_tokens=4),
    ), _input())

    assert message.content == "done"
    assert getattr(message, "token_count", None) is None
    assert [(call.id, call.name, call.arguments) for call in message.tool_calls] == [
        ("call_2", "lookup", '{"q": "y"}'),
    ]
    assert replay == {"native_blocks": [
        {"type": "thinking", "thinking": "plan", "signature": "sig"},
        {"type": "text", "text": "done"},
        {"type": "tool_use", "id": "call_2", "name": "lookup", "input": {"q": "y"}},
    ]}


def test_adapter_assembles_stream_usage_tool_json_and_replay() -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_input(), {"system": True, "tools": False, "messages": True})
    events = []
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="message_start",
        message=SimpleNamespace(usage=SimpleNamespace(
            input_tokens=10,
            output_tokens=1,
            cache_read_input_tokens=5,
            cache_creation_input_tokens=2,
        )),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_start",
        index=0,
        content_block=SimpleNamespace(type="thinking", thinking="", signature=""),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=0,
        delta=SimpleNamespace(type="thinking_delta", thinking="plan"),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_start", index=1,
        content_block=SimpleNamespace(type="tool_use", id="call_2", name="lookup", input={}),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=1,
        delta=SimpleNamespace(type="input_json_delta", partial_json='{"q":"y"}'),
    ), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(type="content_block_stop", index=1), state).events
    events += adapter.push_stream_chunk(SimpleNamespace(
        type="message_delta",
        delta=SimpleNamespace(stop_reason="tool_use"),
        usage=SimpleNamespace(output_tokens=4),
    ), state).events
    finished = adapter.finish_stream(state)

    assert ThinkingDelta(delta="plan") in events
    assert ToolCallEvent(id="call_2", name="lookup", arguments={"q": "y"}) in events
    assert any(isinstance(event, UsageEvent) and event.total_tokens == 21 and event.stop_reason == "tool_use" for event in events)
    assert all(event.cache_read_input_tokens_by_slot is None for event in events if isinstance(event, UsageEvent))
    assert finished.replay == {"native_blocks": [
        {"type": "thinking", "thinking": "plan", "signature": ""},
        {"type": "tool_use", "id": "call_2", "name": "lookup", "input": {"q": "y"}},
    ]}


TEXTUAL_TOOL_CALL_FIXTURE = json.loads(
    (Path(__file__).parents[2] / "tests/fixtures/provider-textual-tool-call/canonical.json")
    .read_text(encoding="utf-8")
)
DSML_START_MARKER = TEXTUAL_TOOL_CALL_FIXTURE["startMarker"]
DSML = TEXTUAL_TOOL_CALL_FIXTURE["complete"]


def test_textual_tool_call_marker_matches_shared_node_python_fixture() -> None:
    assert ANTHROPIC_TEXTUAL_TOOL_CALL_START_MARKER == DSML_START_MARKER


def _compatible_input(with_tools: bool = True):
    resolved = model_registry.resolve_provider_runtime(
        "deepseek", "deepseek-chat", endpoint_id="deepseek.anthropic"
    )
    return normalize_canonical_adapter_input(
        RenderedContext(system_text="", turns=[]),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')]
        if with_tools else [],
        resolved=resolved,
    )


def test_compatible_complete_rejects_exact_dsml_without_body_leakage() -> None:
    with pytest.raises(ProtocolResponseError) as captured:
        AnthropicMessagesAdapter().decode_complete(SimpleNamespace(
            content=[SimpleNamespace(type="text", text=f"preface {DSML}")],
            usage=None,
        ), _compatible_input())

    assert captured.value.provider_code == TEXTUAL_TOOL_CALL_FIXTURE["providerCode"]
    assert captured.value.retryable is True
    assert str(captured.value) == TEXTUAL_TOOL_CALL_FIXTURE["message"]
    assert "secret" not in str(captured.value)


def test_official_no_tools_and_ordinary_xml_text_remain_visible() -> None:
    adapter = AnthropicMessagesAdapter()
    official, _ = adapter.decode_complete(SimpleNamespace(
        content=[SimpleNamespace(type="text", text=DSML)], usage=None,
    ), _input())
    no_tools, _ = adapter.decode_complete(SimpleNamespace(
        content=[SimpleNamespace(type="text", text=DSML)], usage=None,
    ), _compatible_input(with_tools=False))
    ordinary, _ = adapter.decode_complete(SimpleNamespace(
        content=[SimpleNamespace(type="text", text=TEXTUAL_TOOL_CALL_FIXTURE["ordinaryText"])], usage=None,
    ), _compatible_input())

    assert official.content == DSML
    assert no_tools.content == DSML
    assert ordinary.content == TEXTUAL_TOOL_CALL_FIXTURE["ordinaryText"]


def test_native_tool_use_plus_dsml_still_rejects() -> None:
    with pytest.raises(ProtocolResponseError):
        AnthropicMessagesAdapter().decode_complete(SimpleNamespace(content=[
            SimpleNamespace(type="tool_use", id="call_1", name="lookup", input={}),
            SimpleNamespace(type="text", text=DSML),
        ], usage=None), _compatible_input())


def test_explicit_policy_overrides_endpoint_defaults() -> None:
    adapter = AnthropicMessagesAdapter()
    compatible = _compatible_input()
    compatible = type(compatible)(
        context=compatible.context,
        tools=compatible.tools,
        extensions={"textualToolCallPolicy": "off"},
        resolved=compatible.resolved,
    )
    official = _input({"textualToolCallPolicy": "reject"})
    visible, _ = adapter.decode_complete(SimpleNamespace(
        content=[SimpleNamespace(type="text", text=DSML)], usage=None,
    ), compatible)
    assert visible.content == DSML
    with pytest.raises(ProtocolResponseError):
        adapter.decode_complete(SimpleNamespace(
            content=[SimpleNamespace(type="text", text=DSML)], usage=None,
        ), official)
    assert "textualToolCallPolicy" not in adapter.build_request(compatible).params


@pytest.mark.asyncio
async def test_custom_anthropic_base_url_defaults_to_reject_and_policy_stays_off_wire() -> None:
    provider = AnthropicProvider(
        "k",
        base_url="https://gateway.example.test/anthropic",
        retry_config=RetryConfig(max_retries=1, base_delay=0),
    )
    captured: dict = {}

    async def create(**params):
        captured.update(params)
        return SimpleNamespace(content=[SimpleNamespace(type="text", text=DSML)], usage=None)

    provider._client.messages.create = create
    with pytest.raises(ProtocolResponseError) as error:
        await provider.complete(
            RenderedContext(system_text="", turns=[ModelMessage(role="user", content="hi")]),
            [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
        )
    assert error.value.provider_code == "textual_tool_call"
    assert "textualToolCallPolicy" not in captured


@pytest.mark.asyncio
async def test_official_anthropic_provider_defaults_textual_policy_to_off() -> None:
    provider = AnthropicProvider(
        "k",
        retry_config=RetryConfig(max_retries=1, base_delay=0),
    )

    async def create(**_params):
        return SimpleNamespace(content=[SimpleNamespace(type="text", text=DSML)], usage=None)

    provider._client.messages.create = create
    message = await provider.complete(
        RenderedContext(system_text="", turns=[ModelMessage(role="user", content="hi")]),
        [ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')],
    )
    assert message.content == DSML


def test_textual_tool_error_metadata_survives_provider_classification() -> None:
    raw = ProtocolResponseError(
        "anthropic-messages", "safe",
        provider_code="textual_tool_call", retryable=True,
    )
    classified = classify_provider_error("deepseek", raw)
    assert classified.kind == "protocol"
    assert classified.provider_code == "textual_tool_call"
    assert classified.retryable is True


def test_complete_usage_marks_cache_telemetry_as_measured_or_unavailable() -> None:
    adapter = AnthropicMessagesAdapter()
    measured = adapter.normalize_usage({
        "input_tokens": 10,
        "output_tokens": 2,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
    })
    unavailable = adapter.normalize_usage({"input_tokens": 10, "output_tokens": 2})

    assert measured is not None
    assert measured.cache_telemetry_status == "measured"
    assert measured.cache_telemetry_source == "anthropic_usage"
    assert unavailable is not None
    assert unavailable.cache_telemetry_status == "unavailable"
    assert unavailable.cache_telemetry_source is None


def test_usage_rejects_non_integer_counts() -> None:
    with pytest.raises(ValueError, match="integer"):
        AnthropicMessagesAdapter().normalize_usage({"input_tokens": 1.5, "output_tokens": 0})


def test_stream_cache_telemetry_is_unavailable_when_cache_fields_are_absent() -> None:
    adapter = AnthropicMessagesAdapter()
    unavailable_state = adapter.create_stream_state(_input())
    unavailable = adapter.push_stream_chunk(SimpleNamespace(
        type="message_start",
        message=SimpleNamespace(usage=SimpleNamespace(input_tokens=10, output_tokens=0)),
    ), unavailable_state).events[-1]
    assert unavailable.cache_telemetry_status == "unavailable"
    assert unavailable.cache_telemetry_source is None

    measured_state = adapter.create_stream_state(_input())
    measured = adapter.push_stream_chunk(SimpleNamespace(
        type="message_start",
        message=SimpleNamespace(usage=SimpleNamespace(
            input_tokens=10,
            output_tokens=0,
            cache_read_input_tokens=0,
            cache_creation_input_tokens=0,
        )),
    ), measured_state).events[-1]
    assert measured.cache_telemetry_status == "measured"
    assert measured.cache_telemetry_source == "anthropic_usage"


@pytest.mark.parametrize("split", range(len(DSML_START_MARKER) + 1))
def test_stream_buffers_every_marker_split_without_leaking_candidate(split: int) -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_compatible_input())
    visible: list[str] = []
    for text in (
        f"visible:{DSML_START_MARKER[:split]}",
        f"{DSML_START_MARKER[split:]}secret-body",
    ):
        output = adapter.push_stream_chunk(SimpleNamespace(
            type="content_block_delta", index=0,
            delta=SimpleNamespace(type="text_delta", text=text),
        ), state)
        visible.extend(event.delta for event in output.events if hasattr(event, "delta"))

    with pytest.raises(ProtocolResponseError) as captured:
        adapter.finish_stream(state)
    assert "".join(visible) == "visible:"
    assert "secret-body" not in str(captured.value)


def test_stream_flushes_harmless_marker_prefix_at_eof() -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_compatible_input())
    pushed = adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=0,
        delta=SimpleNamespace(type="text_delta", text=f"plain {DSML_START_MARKER[:-1]}"),
    ), state)
    finished = adapter.finish_stream(state)
    visible = [event.delta for event in [*pushed.events, *finished.events] if hasattr(event, "delta")]
    assert "".join(visible) == f"plain {DSML_START_MARKER[:-1]}"


def test_stream_detects_marker_split_between_block_start_and_delta() -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_compatible_input())
    split = 8
    started = adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_start", index=0,
        content_block=SimpleNamespace(
            type="text", text=f"visible:{DSML_START_MARKER[:split]}"
        ),
    ), state)
    continued = adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=0,
        delta=SimpleNamespace(
            type="text_delta", text=f"{DSML_START_MARKER[split:]}secret-body"
        ),
    ), state)
    visible = [
        event.delta for event in [*started.events, *continued.events]
        if hasattr(event, "delta")
    ]
    assert "".join(visible) == "visible:"
    with pytest.raises(ProtocolResponseError):
        adapter.finish_stream(state)


def test_stream_candidate_capture_bound_uses_the_same_safe_error() -> None:
    adapter = AnthropicMessagesAdapter()
    state = adapter.create_stream_state(_compatible_input())
    adapter.push_stream_chunk(SimpleNamespace(
        type="content_block_delta", index=0,
        delta=SimpleNamespace(type="text_delta", text=DSML_START_MARKER),
    ), state)
    with pytest.raises(ProtocolResponseError) as captured:
        adapter.push_stream_chunk(SimpleNamespace(
            type="content_block_delta", index=0,
            delta=SimpleNamespace(type="text_delta", text="😀" * 20_000),
        ), state)
    assert str(captured.value) == TEXTUAL_TOOL_CALL_FIXTURE["message"]
