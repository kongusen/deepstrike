"""spc_016-03 provider-owned OAuth credential chain contracts."""
from __future__ import annotations

import asyncio

import pytest

from deepstrike.providers.credentials import (
    CredentialResolutionError,
    OAuthCredentialResolver,
)
from deepstrike.providers.runtime_registry import create_provider_async


REQUEST = {
    "provider_id": "openai",
    "model_id": "gpt-4.1",
    "endpoint_id": "openai.responses",
    "protocol": "openai-responses",
}


@pytest.mark.asyncio
async def test_oauth_refresh_is_deduplicated_and_repeats_only_after_expiry() -> None:
    now = 1_000
    refreshes = 0
    started = asyncio.Event()
    release = asyncio.Event()

    async def refresh():
        nonlocal refreshes
        refreshes += 1
        started.set()
        await release.wait()
        return {
            "access_token": f"access-{refreshes}",
            "expires_at": now + 10,
            "audience": "https://api.openai.com",
            "scopes": ["responses.write"],
        }

    resolver = OAuthCredentialResolver(
        provider_id="openai",
        audience="https://api.openai.com",
        required_scopes=("responses.write",),
        clock=lambda: now,
        refresh=refresh,
    )
    pending = [asyncio.create_task(resolver.resolve(**REQUEST)) for _ in range(3)]
    await started.wait()
    assert refreshes == 1
    release.set()
    assert await asyncio.gather(*pending) == ["access-1", "access-1", "access-1"]

    now += 11
    assert await resolver.resolve(**REQUEST) == "access-2"
    assert refreshes == 2


@pytest.mark.asyncio
async def test_oauth_scope_audience_revocation_and_refresh_errors_fail_closed_without_secret() -> None:
    secret = "oauth-refresh-secret"
    wrong_scope = OAuthCredentialResolver(
        provider_id="openai",
        required_scopes=("responses.write",),
        clock=lambda: 0,
        refresh=lambda: _token(secret, scopes=("read",)),
    )
    with pytest.raises(CredentialResolutionError) as scope_error:
        await wrong_scope.resolve(**REQUEST)
    assert scope_error.value.code == "credential_oauth_scope_mismatch"
    assert not scope_error.value.retryable

    wrong_audience = OAuthCredentialResolver(
        provider_id="openai",
        audience="https://api.openai.com",
        clock=lambda: 0,
        refresh=lambda: _token(secret, audience="https://other.example"),
    )
    with pytest.raises(CredentialResolutionError) as audience_error:
        await wrong_audience.resolve(**REQUEST)
    assert audience_error.value.code == "credential_oauth_audience_mismatch"
    assert not audience_error.value.retryable

    revoked = OAuthCredentialResolver(provider_id="openai", clock=lambda: 0, refresh=lambda: _token(secret))
    revoked.revoke()
    with pytest.raises(CredentialResolutionError) as revoked_error:
        await revoked.resolve(**REQUEST)
    assert revoked_error.value.code == "credential_revoked"
    assert not revoked_error.value.retryable

    async def fail_refresh():
        raise RuntimeError(secret)

    failure = OAuthCredentialResolver(provider_id="openai", clock=lambda: 0, refresh=fail_refresh)
    with pytest.raises(CredentialResolutionError) as refresh_error:
        await failure.resolve(**REQUEST)
    assert refresh_error.value.code == "credential_refresh_failed"
    assert refresh_error.value.retryable
    assert secret not in str(refresh_error.value)
    assert secret not in repr(refresh_error.value)


@pytest.mark.asyncio
async def test_runtime_factory_receives_only_refreshed_bearer_credential() -> None:
    secret = "oauth-access-token"
    resolver = OAuthCredentialResolver(
        provider_id="openai",
        clock=lambda: 0,
        refresh=lambda: _token(secret),
    )
    provider = await create_provider_async(
        "openai",
        model="gpt-4.1",
        protocol="responses",
        credential_resolver=resolver,
    )

    assert provider._client.api_key == secret
    assert secret not in repr(provider._resolved_runtime)
    assert provider.requestPlanIdentity() == {
        "providerId": "openai",
        "modelId": "gpt-4.1",
        "endpoint": {
            "id": "openai.responses",
            "protocol": "openai-responses",
            "baseURL": "https://api.openai.com/v1",
        },
    }
    assert secret not in repr(provider.requestPlanIdentity())
    assert resolver.status() == {
        "provider_id": "openai",
        "revoked": False,
        "has_usable_token": True,
    }


@pytest.mark.asyncio
async def test_oauth_bearer_policy_is_not_inherited_by_compatible_providers() -> None:
    resolver = OAuthCredentialResolver(
        provider_id="deepseek",
        clock=lambda: 0,
        refresh=lambda: _token("deepseek-token"),
    )
    with pytest.raises(CredentialResolutionError) as error:
        await create_provider_async(
            "deepseek",
            model="deepseek-chat",
            credential_resolver=resolver,
        )
    assert error.value.code == "credential_auth_mode_unsupported"
    assert not error.value.retryable


async def _token(
    access_token: str,
    *,
    expires_at: int = 10_000,
    audience: str | None = None,
    scopes: tuple[str, ...] = (),
) -> dict[str, object]:
    return {
        "access_token": access_token,
        "expires_at": expires_at,
        **({"audience": audience} if audience is not None else {}),
        **({"scopes": list(scopes)} if scopes else {}),
    }
