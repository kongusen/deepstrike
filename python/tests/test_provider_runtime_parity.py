"""P-00 Red tests for SPC-014 Python Model Runtime Parity.

These tests intentionally fail against the current implementation to prove the gaps
that P-08 (ProviderError) and P-09 (stop reason) will close.
"""
from __future__ import annotations

import httpx
import pytest

from deepstrike.providers.provider_error import (
    ProviderError,
    classify_provider_error,
    provider_error_event_fields,
)
from deepstrike.providers.stop_reason import canonicalize_stop_reason
from deepstrike.providers.usage import ProviderUsage, normalize_usage
from deepstrike.providers.model_registry import (
    CapabilityState,
    EffectiveCapability,
    ModelRegistry,
    model_registry,
    resolve_effective_capabilities,
)


class FakeOpenAIError(Exception):
    """Stand-in for openai.APIStatusError when we do not want to import the SDK."""

    def __init__(self, message: str, *, code: str | None, status_code: int):
        super().__init__(message)
        self.code = code
        self.status_code = status_code
        self.response = type("Response", (), {"status_code": status_code})()


class FakeAnthropicError(Exception):
    """Stand-in for anthropic.APIStatusError."""

    def __init__(self, message: str, *, code: str | None, status_code: int):
        super().__init__(message)
        self.code = code
        self.status_code = status_code
        self.response = type("Response", (), {"status_code": status_code})()


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("stop", "end_turn"),
        ("length", "max_tokens"),
        ("tool_calls", "tool_use"),
        ("function_call", "tool_use"),
        ("content_filter", "content_filter"),
        ("unknown_reason", "other"),
        ("max_output_tokens", "max_tokens"),
        ("STOP", "end_turn"),
        ("SAFETY", "content_filter"),
        ("end_turn", "end_turn"),
        ("tool_use", "tool_use"),
        ("max_tokens", "max_tokens"),
        ("stop_sequence", "stop_sequence"),
    ],
)
def test_canonicalize_stop_reason(raw: str, expected: str) -> None:
    assert canonicalize_stop_reason(raw) == expected


def test_context_length_exceeded_is_context_overflow() -> None:
    exc = FakeOpenAIError(
        "context length exceeded",
        code="context_length_exceeded",
        status_code=400,
    )
    err = classify_provider_error("openai", exc)
    assert err.kind == "context_overflow"
    assert err.retryable is False
    assert err.http_status == 400
    assert err.provider_code == "context_length_exceeded"
    assert err.provider == "openai"


def test_anthropic_prompt_too_long_is_context_overflow() -> None:
    exc = FakeAnthropicError(
        "prompt is too long",
        code="prompt_too_long",
        status_code=400,
    )
    err = classify_provider_error("anthropic", exc)
    assert err.kind == "context_overflow"
    assert err.provider == "anthropic"


def test_auth_error_is_not_retryable() -> None:
    exc = FakeOpenAIError("Unauthorized", code="invalid_api_key", status_code=401)
    err = classify_provider_error("openai", exc)
    assert err.kind == "auth"
    assert err.retryable is False
    assert err.http_status == 401


def test_rate_limit_is_retryable() -> None:
    exc = FakeOpenAIError("Rate limit", code="rate_limit_exceeded", status_code=429)
    err = classify_provider_error("openai", exc)
    assert err.kind == "rate_limit"
    assert err.retryable is True
    assert err.http_status == 429


def test_model_unavailable_5xx_is_retryable() -> None:
    exc = FakeOpenAIError("Server error", code="server_error", status_code=503)
    err = classify_provider_error("openai", exc)
    assert err.kind == "model_unavailable"
    assert err.retryable is True
    assert err.http_status == 503


def test_network_error_is_transport() -> None:
    exc = httpx.ConnectError("Connection refused")
    err = classify_provider_error("openai", exc)
    assert err.kind == "transport"
    assert err.retryable is True


def test_provider_error_event_fields_exclude_cause_and_response() -> None:
    exc = FakeOpenAIError(
        "context length exceeded",
        code="context_length_exceeded",
        status_code=400,
    )
    err = classify_provider_error("openai", exc)
    fields = provider_error_event_fields(err)
    assert fields == {
        "error_kind": "context_overflow",
        "retryable": False,
        "http_status": 400,
        "provider_code": "context_length_exceeded",
    }
    # cause / raw response must never leak into the host event
    assert "cause" not in fields
    assert "response" not in fields


def test_bare_exception_has_unknown_kind_and_no_sensitive_fields() -> None:
    err = classify_provider_error("custom", ValueError("something else"))
    assert err.kind == "unknown"
    fields = provider_error_event_fields(err)
    assert fields == {"error_kind": "unknown", "retryable": False}


def test_normalize_usage_missing_returns_none() -> None:
    assert normalize_usage(None) is None
    assert normalize_usage({}) is None


def test_normalize_usage_openai_shape() -> None:
    raw = {
        "prompt_tokens": 10,
        "completion_tokens": 5,
        "total_tokens": 15,
        "prompt_tokens_details": {"cached_tokens": 3},
    }
    usage = normalize_usage(raw)
    assert usage == ProviderUsage(
        input_tokens=10,
        output_tokens=5,
        cache_read_input_tokens=3,
        cache_creation_input_tokens=0,
    )


def test_normalize_usage_anthropic_shape() -> None:
    raw = {
        "input_tokens": 20,
        "output_tokens": 7,
        "cache_read_input_tokens": 4,
        "cache_creation_input_tokens": 2,
    }
    usage = normalize_usage(raw)
    assert usage == ProviderUsage(
        input_tokens=20,
        output_tokens=7,
        cache_read_input_tokens=4,
        cache_creation_input_tokens=2,
    )


def test_normalize_usage_rejects_non_numeric() -> None:
    with pytest.raises(ProviderError) as exc:
        normalize_usage({"input_tokens": "ten"})
    assert exc.value.kind == "protocol"


def test_normalize_usage_rejects_negative() -> None:
    with pytest.raises(ProviderError) as exc:
        normalize_usage({"input_tokens": -1, "output_tokens": 0})
    assert exc.value.kind == "protocol"


def test_normalize_usage_reasoning_tokens() -> None:
    raw = {
        "input_tokens": 10,
        "output_tokens": 8,
        "output_tokens_details": {"reasoning_tokens": 3},
    }
    usage = normalize_usage(raw)
    assert usage == ProviderUsage(
        input_tokens=10,
        output_tokens=8,
        reasoning_tokens=3,
    )


def test_registry_resolves_known_openai_chat() -> None:
    reg = model_registry.resolve("openai/gpt-4o")
    assert reg is not None
    assert reg.descriptor.provider_id == "openai"
    assert reg.default_endpoint_id == "openai.chat"
    assert reg.descriptor.kind == "generation"


def test_registry_resolves_openai_responses_model() -> None:
    reg = model_registry.resolve("openai/gpt-5.4")
    assert reg is not None
    assert reg.default_endpoint_id == "openai.responses"


def test_registry_resolves_openai_embedding() -> None:
    reg = model_registry.resolve("openai/text-embedding-3-small")
    assert reg is not None
    assert reg.default_endpoint_id == "openai.embeddings"
    assert reg.descriptor.kind == "embedding"


def test_registry_resolves_unknown_provider_fail_open() -> None:
    assert model_registry.resolve("custom/unknown") is None


def test_registry_resolves_with_explicit_provider() -> None:
    reg = model_registry.resolve("claude-sonnet-4-6", provider_id="anthropic")
    assert reg is not None
    assert reg.descriptor.provider_id == "anthropic"
    assert reg.default_endpoint_id == "anthropic.messages"


def test_registry_runtime_policy_for_known_model() -> None:
    reg = model_registry.resolve("openai/gpt-4o")
    assert reg.recommended_runtime_policy is not None
    assert reg.recommended_runtime_policy.max_turns == 25


def test_registry_unknown_model_has_no_policy() -> None:
    reg = model_registry.resolve("openai/unknown-model")
    assert reg is not None
    assert reg.recommended_runtime_policy is None


def test_effective_capabilities_tri_state_unknown_model() -> None:
    runtime = model_registry.resolve_provider_runtime("openai", "unknown-model")
    assert runtime.model is not None
    assert runtime.protocol == "openai-chat"
    # Protocol supports tools, model intrinsic is unknown -> effective stays unknown.
    assert runtime.effective_capabilities.tools.state == "unknown"


def test_effective_capabilities_known_tools_supported() -> None:
    from deepstrike.providers.model_registry import ModelDescriptor
    model = ModelDescriptor(
        id="openai/gpt-4o",
        provider_id="openai",
        kind="generation",
        intrinsic_tools=True,
    )
    caps = resolve_effective_capabilities(model, "openai.chat")
    assert caps.tools == EffectiveCapability(state="supported", value=True, evidence=("model", "protocol"))


def test_native_token_counting_only_on_verified_endpoints() -> None:
    runtime = model_registry.resolve_provider_runtime("anthropic", "claude-sonnet-4-6")
    assert runtime.effective_capabilities.native_token_counting.state == "supported"
    runtime = model_registry.resolve_provider_runtime("openai", "gpt-4o")
    assert runtime.effective_capabilities.native_token_counting.state == "unknown"


def test_registry_provider_prefix_inference() -> None:
    reg = model_registry.resolve("anthropic/claude-opus-4-1")
    assert reg is not None
    assert reg.default_endpoint_id == "anthropic.messages"


def test_provider_error_retains_cause_internally() -> None:
    original = FakeOpenAIError("boom", code="x", status_code=500)
    err = classify_provider_error("openai", original)
    assert err.__cause__ is original or err.__context__ is original
