"""Cross-SDK provider runtime service contracts for SPC-015/016."""
from __future__ import annotations

import pytest

from deepstrike.providers.credentials import (
    CredentialResolutionError,
    ProviderCredential,
    redact_credential,
    resolve_credential,
)
from deepstrike.providers.model_catalog import DynamicModelCatalog, StaticModelCatalog
from deepstrike.providers.model_registry import ModelDescriptor, ModelRegistration
from deepstrike.providers.capability_router import CapabilityRequirement, CapabilityRouter, ProviderCandidate


def _registration(
    identifier: str,
    *,
    input_modalities: tuple[str, ...] = (),
    tools: bool | None = None,
    reasoning: bool | None = None,
    context_window: int | None = None,
) -> ModelRegistration:
    return ModelRegistration(
        descriptor=ModelDescriptor(
            id=identifier,
            provider_id="openai",
            kind="generation",
            context_window=context_window,
            intrinsic_input_modalities=input_modalities,
            intrinsic_tools=tools,
            intrinsic_reasoning=reasoning,
        ),
        default_endpoint_id="openai.chat",
    )


@pytest.mark.asyncio
async def test_custom_credential_resolver_is_validated_redacted_and_fails_closed() -> None:
    request = {
        "provider_id": "openai",
        "model_id": "gpt-4o",
        "endpoint_id": "openai.chat",
        "protocol": "openai-chat",
    }
    secret = "resolver-secret"
    credential = await resolve_credential(
        **request,
        credential_resolver=lambda _request: ProviderCredential("api_key", secret),
    )
    assert credential == ProviderCredential("api_key", secret)
    assert redact_credential(credential) == {"type": "api_key"}

    with pytest.raises(CredentialResolutionError) as failure:
        await resolve_credential(
            **request,
            credential_resolver=lambda _request: ProviderCredential("api_key", " "),
        )
    assert failure.value.code == "credential_invalid"
    assert secret not in str(failure.value)
    assert secret not in repr(failure.value.__dict__)


@pytest.mark.asyncio
async def test_dynamic_catalog_keeps_static_and_last_good_snapshot_after_refresh_failure() -> None:
    class Source:
        fail = False

        async def list(self):
            if self.fail:
                raise RuntimeError("offline")
            return [_registration("openai/dynamic")]

    source = Source()
    catalog = DynamicModelCatalog(source, StaticModelCatalog([_registration("openai/static")]))
    assert await catalog.refresh() == {"ok": True}
    source.fail = True
    assert await catalog.refresh() == {"ok": False, "error_code": "refresh_failed"}
    assert (await catalog.get("openai/dynamic")).descriptor.id == "openai/dynamic"
    assert (await catalog.get("openai/static")).descriptor.id == "openai/static"
    assert [row.descriptor.id for row in await catalog.list()] == ["openai/dynamic", "openai/static"]


@pytest.mark.asyncio
async def test_async_provider_construction_uses_catalog_facts_for_raw_and_canonical_model_ids() -> None:
    catalog = StaticModelCatalog([_registration("openai/private", tools=True)])
    from deepstrike.providers.runtime_registry import create_provider_async

    raw = await create_provider_async("openai", model="private", api_key="key", model_catalog=catalog)
    canonical = await create_provider_async("openai", model="openai/private", api_key="key", model_catalog=catalog)
    assert raw._resolved_runtime.model.id == canonical._resolved_runtime.model.id == "openai/private"
    assert raw._model == canonical._model == "private"


@pytest.mark.asyncio
async def test_capability_router_uses_selected_runtime_and_returns_secret_free_no_match() -> None:
    catalog = StaticModelCatalog([
        _registration("openai/text", input_modalities=("text",), tools=False, reasoning=False, context_window=8_000),
        _registration("openai/vision", input_modalities=("text", "image"), tools=True, reasoning=True, context_window=128_000),
        _registration("openai/unknown"),
    ])
    candidates = [
        ProviderCandidate(provider_id="openai", model=model, api_key="secret", model_catalog=catalog)
        for model in ("text", "vision", "unknown")
    ]

    selected = await CapabilityRouter().route(CapabilityRequirement(
        required_input_modalities=("image",), tools=True, reasoning=True, minimum_context_window=64_000,
    ), candidates)
    assert selected.ok is True
    assert selected.runtime is not None
    assert selected.runtime.model_id == "vision"
    assert getattr(selected.provider, "_resolved_runtime") is selected.runtime

    unknown = await CapabilityRouter().route(
        CapabilityRequirement(tools=True, reasoning=True), [candidates[2]],
    )
    assert unknown.ok is True

    missed = await CapabilityRouter().route(
        CapabilityRequirement(required_input_modalities=("audio",), minimum_context_window=1_000_000),
        [candidates[0]],
    )
    assert missed.ok is False
    assert missed.error == {
        "code": "no_capable_model",
        "requirement": {"required_input_modalities": ("audio",), "minimum_context_window": 1_000_000},
        "candidates": [{"model": "text", "rejected_by": ["input:audio", "context"]}],
    }
    assert "secret" not in repr(missed.error)
