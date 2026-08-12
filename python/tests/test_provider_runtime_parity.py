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


def test_provider_error_retains_cause_internally() -> None:
    original = FakeOpenAIError("boom", code="x", status_code=500)
    err = classify_provider_error("openai", original)
    assert err.__cause__ is original or err.__context__ is original
