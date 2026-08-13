"""Structured provider-failure contract.

Only the scalar fields exposed by `provider_error_event_fields` cross the runner → canonical
host ABI. The original SDK exception is retained as ``__cause__`` for Node-side diagnostics
but is never serialized into the event or forwarded to the kernel.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Literal

ProviderErrorKind = Literal[
    "transport",
    "auth",
    "rate_limit",
    "context_overflow",
    "invalid_request",
    "modality",
    "model_unavailable",
    "protocol",
    "unknown",
]

CONTEXT_OVERFLOW_CODES = frozenset({
    "context_length_exceeded",
    "prompt_too_long",
})

NETWORK_CODES = frozenset({
    "ECONNABORTED",
    "ECONNREFUSED",
    "ECONNRESET",
    "EHOSTUNREACH",
    "ENETUNREACH",
    "ETIMEDOUT",
})


@dataclass
class ProviderError(Exception):
    """Stable provider-failure contract."""

    provider: str
    kind: ProviderErrorKind
    retryable: bool
    message: str
    http_status: int | None = None
    provider_code: str | None = None
    cause: Any = field(repr=False, default=None)

    def __post_init__(self) -> None:
        Exception.__init__(self, self.message)


def _as_object(value: Any) -> dict[str, Any] | None:
    return value if isinstance(value, dict) else None


def _scalar_status(value: Any) -> int | None:
    if isinstance(value, int) and 100 <= value <= 599:
        return value
    return None


def _error_status(error: Any) -> int | None:
    outer = _as_object(error)
    response = _as_object(getattr(error, "response", None))
    return (
        _scalar_status(getattr(error, "http_status", None))
        or _scalar_status(getattr(error, "status", None))
        or _scalar_status(getattr(error, "status_code", None))
        or _scalar_status(outer.get("httpStatus") if outer else None)
        or _scalar_status(outer.get("status") if outer else None)
        or _scalar_status(getattr(response, "status", None))
        or _scalar_status(getattr(response, "status_code", None))
    )


def _scalar_string(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _error_code(error: Any) -> str | None:
    outer = _as_object(error)
    nested = _as_object(outer.get("error")) if outer else None
    nested_error = _as_object(nested.get("error")) if nested else None
    body = _as_object(outer.get("body")) if outer else None
    body_error = _as_object(body.get("error")) if body else None

    return (
        _scalar_string(getattr(error, "providerCode", None))
        or _scalar_string(getattr(error, "code", None))
        or _scalar_string(getattr(error, "error_code", None))
        or _scalar_string(outer.get("providerCode") if outer else None)
        or _scalar_string(outer.get("code") if outer else None)
        or _scalar_string(outer.get("error_code") if outer else None)
        or _scalar_string(nested.get("code") if nested else None)
        or _scalar_string(nested.get("error_code") if nested else None)
        or _scalar_string(nested_error.get("code") if nested_error else None)
        or _scalar_string(nested_error.get("error_code") if nested_error else None)
        or _scalar_string(nested_error.get("type") if nested_error else None)
        or _scalar_string(body_error.get("code") if body_error else None)
        or _scalar_string(body_error.get("error_code") if body_error else None)
    )


def _error_message(error: Any) -> str:
    if isinstance(error, Exception):
        msg = str(error)
        if msg:
            return msg
    return str(error) if isinstance(error, str) else "Provider request failed"


def _class_name(error: Any) -> str | None:
    return type(error).__name__ if isinstance(error, Exception) else None


def _classify_kind(error: Any, status: int | None, code: str | None) -> ProviderErrorKind:
    if status == 413 or (code and code.lower() in CONTEXT_OVERFLOW_CODES):
        return "context_overflow"

    name = _class_name(error)
    if name == "UnsupportedModalityError":
        return "modality"
    if name == "ProtocolResponseError":
        return "protocol"
    if name in {"ContentValidationError", "ProviderReplayValidationError"}:
        return "invalid_request"
    if name in {"APIConnectionError", "APIConnectionTimeoutError", "ConnectError", "ConnectTimeout", "ReadTimeout", "WriteTimeout", "TimeoutException"}:
        return "transport"

    if status == 401 or status == 403:
        return "auth"
    if status == 429:
        return "rate_limit"
    if code == "model_not_found" or status == 404 or (status is not None and status >= 500):
        return "model_unavailable"
    if status == 408 or status == 409:
        return "transport"
    if status == 400 or status == 422:
        return "invalid_request"

    normalized_code = code.upper() if code else None
    if normalized_code and normalized_code in NETWORK_CODES:
        return "transport"
    if isinstance(error, TypeError) and status is None:
        return "transport"
    return "unknown"


def _retryable(kind: ProviderErrorKind, status: int | None) -> bool:
    if kind in {"transport", "rate_limit", "model_unavailable"}:
        return True
    if kind == "unknown" and status is not None:
        return status >= 500 and status != 501
    return False


def classify_provider_error(provider: str, error: Any) -> ProviderError:
    """Wrap a raw SDK/transport exception in a stable ``ProviderError``."""
    if isinstance(error, ProviderError):
        return error

    http_status = _error_status(error)
    provider_code = _error_code(error)
    kind = _classify_kind(error, http_status, provider_code)
    err = ProviderError(
        provider=provider,
        kind=kind,
        retryable=_retryable(kind, http_status),
        message=_error_message(error),
        http_status=http_status,
        provider_code=provider_code,
        cause=error,
    )
    err.__cause__ = error
    return err


def provider_error_event_fields(error: Any) -> dict[str, Any]:
    """Project only the safe scalar fields that may cross the host ABI."""
    if not isinstance(error, ProviderError):
        return {}
    fields: dict[str, Any] = {
        "error_kind": error.kind,
        "retryable": error.retryable,
    }
    if error.http_status is not None:
        fields["http_status"] = error.http_status
    if error.provider_code is not None:
        fields["provider_code"] = error.provider_code
    return fields


def canonical_provider_failure_kind(kind: str | None) -> str:
    """Fold provider diagnostic taxonomy into the kernel's closed failure vocabulary."""
    if kind in {"transport", "rate_limit", "model_unavailable"}:
        return "transport_exhausted"
    if kind in {"auth", "invalid_request", "modality", "protocol"}:
        return "protocol_error"
    return "unknown"
