"""SPC-024 executable token-measurement capability contracts."""

import pytest

from deepstrike.providers.model_registry import TOKEN_MEASUREMENT_EVIDENCE, model_registry
from deepstrike.providers.runtime_registry import create_provider


def test_token_measurement_evidence_describes_api_and_adapter_support() -> None:
    for entry in TOKEN_MEASUREMENT_EVIDENCE:
        assert entry["source"].startswith("https://")
        assert entry["verified_at"] == "2026-08-26"
        assert entry["provider_api_state"] in {"supported", "unsupported", "unknown"}
        assert entry["adapter_state"] in {"available", "unavailable"}
        assert entry["method"] in {
            "provider_preflight", "official_local_tokenizer", "postflight", "heuristic"
        }
        assert entry["coverage"]
        assert entry["sdk"]


@pytest.mark.parametrize(
    ("provider_id", "model_id", "endpoint_id"),
    [
        ("anthropic", "claude-sonnet-4-6", "anthropic.messages"),
        ("gemini", "gemini-2.5-pro", "gemini.google"),
    ],
)
def test_runtime_native_token_counting_is_executable(
    provider_id: str, model_id: str, endpoint_id: str
) -> None:
    runtime = model_registry.resolve_provider_runtime(provider_id, model_id, endpoint_id=endpoint_id)
    provider = create_provider(provider_id, api_key="test", model=model_id)

    if runtime.effective_capabilities.native_token_counting.state == "supported":
        assert callable(getattr(provider, "count_tokens", None))


def test_openai_responses_native_token_counting_is_executable() -> None:
    runtime = model_registry.resolve_provider_runtime("openai", "gpt-5.2", endpoint_id="openai.responses")
    assert runtime.effective_capabilities.native_token_counting.state == "supported"
    provider = create_provider("openai", api_key="test", model="gpt-5.2", protocol="responses")
    assert callable(getattr(provider, "count_tokens", None))


def test_openai_chat_does_not_inherit_responses_counting() -> None:
    runtime = model_registry.resolve_provider_runtime("openai", "gpt-4o")
    assert runtime.effective_capabilities.native_token_counting.state == "unknown"
    provider = create_provider("openai", api_key="test", model="gpt-4o")
    assert not callable(getattr(provider, "count_tokens", None))

