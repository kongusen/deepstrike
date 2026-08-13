"""Host-owned provider credential resolution, including the OAuth extension chain.

No credential object in this module is Kernel data.  OAuth refresh transport and any refresh
token remain in the caller-supplied callback; this module retains only the current access token.
"""
from __future__ import annotations

import asyncio
import inspect
import time
from dataclasses import dataclass
from typing import Awaitable, Callable, Literal, TypedDict


CredentialKind = Literal["api_key", "bearer"]


class CredentialResolutionError(Exception):
    """Secret-free, structured credential failure for the provider Host boundary."""

    def __init__(self, code: str, provider_id: str, *, retryable: bool = False) -> None:
        self.code = code
        self.provider_id = provider_id
        self.retryable = retryable
        super().__init__(_message(code, provider_id))


class OAuthAccessToken(TypedDict, total=False):
    access_token: str
    expires_at: float
    scopes: list[str]
    audience: str


OAuthRefresh = Callable[[], OAuthAccessToken | Awaitable[OAuthAccessToken]]


@dataclass(frozen=True)
class ProviderCredential:
    type: CredentialKind
    value: str


class CredentialRequest(TypedDict):
    provider_id: str
    model_id: str
    endpoint_id: str
    protocol: str


CredentialResolver = Callable[
    [CredentialRequest],
    ProviderCredential | None | Awaitable[ProviderCredential | None],
]


class OAuthCredentialResolver:
    """Refreshes a provider-owned bearer token once per expiry window.

    ``refresh`` is intentionally a narrow, Host-owned callback.  It receives no Kernel state and
    returns an access-token snapshot only; refresh tokens, SDK clients and HTTP mechanics never
    cross this boundary.
    """

    def __init__(
        self,
        *,
        provider_id: str,
        refresh: OAuthRefresh,
        required_scopes: tuple[str, ...] = (),
        audience: str | None = None,
        clock: Callable[[], float] | None = None,
    ) -> None:
        self._provider_id = provider_id
        self._refresh = refresh
        self._required_scopes = tuple(required_scopes)
        self._audience = audience
        self._clock = clock or time.time
        self._token: OAuthAccessToken | None = None
        self._refresh_in_flight: asyncio.Task[OAuthAccessToken] | None = None
        self._revoked = False

    async def resolve(
        self,
        *,
        provider_id: str,
        model_id: str,
        endpoint_id: str,
        protocol: str,
    ) -> str:
        del model_id, endpoint_id, protocol
        if provider_id != self._provider_id:
            raise CredentialResolutionError("credential_unavailable", provider_id)
        if self._revoked:
            raise CredentialResolutionError("credential_revoked", self._provider_id)
        token = self._token if self._is_usable(self._token) else await self._refresh_token()
        self._validate(token)
        if self._revoked:
            raise CredentialResolutionError("credential_revoked", self._provider_id)
        return token["access_token"]

    def revoke(self) -> None:
        self._revoked = True
        self._token = None

    def status(self) -> dict[str, object]:
        return {
            "provider_id": self._provider_id,
            "revoked": self._revoked,
            "has_usable_token": not self._revoked and self._is_usable(self._token),
        }

    def _is_usable(self, token: OAuthAccessToken | None) -> bool:
        return bool(
            token
            and isinstance(token.get("expires_at"), (int, float))
            and not isinstance(token.get("expires_at"), bool)
            and token["expires_at"] > self._clock()
        )

    async def _refresh_token(self) -> OAuthAccessToken:
        if self._refresh_in_flight is None:
            self._refresh_in_flight = asyncio.create_task(self._run_refresh())
        try:
            return await self._refresh_in_flight
        finally:
            if self._refresh_in_flight is not None and self._refresh_in_flight.done():
                self._refresh_in_flight = None

    async def _run_refresh(self) -> OAuthAccessToken:
        try:
            token = self._refresh()
            if inspect.isawaitable(token):
                token = await token
            self._validate(token)
            if self._revoked:
                raise CredentialResolutionError("credential_revoked", self._provider_id)
            self._token = token
            return token
        except CredentialResolutionError:
            raise
        except Exception as exc:
            raise CredentialResolutionError(
                "credential_refresh_failed", self._provider_id, retryable=True
            ) from None

    def _validate(self, token: OAuthAccessToken | None) -> None:
        if not token or not isinstance(token.get("access_token"), str) or not token["access_token"].strip():
            raise CredentialResolutionError("credential_invalid", self._provider_id)
        if not self._is_usable(token):
            raise CredentialResolutionError("credential_invalid", self._provider_id)
        if self._audience is not None and token.get("audience") != self._audience:
            raise CredentialResolutionError("credential_oauth_audience_mismatch", self._provider_id)
        scopes = token.get("scopes") or []
        if not isinstance(scopes, list) or any(scope not in scopes for scope in self._required_scopes):
            raise CredentialResolutionError("credential_oauth_scope_mismatch", self._provider_id)


async def resolve_credential(
    *,
    provider_id: str,
    model_id: str,
    endpoint_id: str,
    protocol: str,
    api_key: str | None = None,
    bearer_token: str | None = None,
    credential_resolver: CredentialResolver | OAuthCredentialResolver | None = None,
) -> ProviderCredential:
    configured = [
        ProviderCredential("api_key", api_key) if api_key is not None else None,
        ProviderCredential("bearer", bearer_token) if bearer_token is not None else None,
    ]
    configured = [credential for credential in configured if credential is not None]
    if len(configured) > 1:
        raise CredentialResolutionError("credential_invalid", provider_id)
    if configured:
        credential = configured[0]
        if not credential.value.strip():
            raise CredentialResolutionError("credential_invalid", provider_id)
        return credential
    if credential_resolver is None:
        raise CredentialResolutionError("credential_unavailable", provider_id)
    request: CredentialRequest = {
        "provider_id": provider_id,
        "model_id": model_id,
        "endpoint_id": endpoint_id,
        "protocol": protocol,
    }
    try:
        if isinstance(credential_resolver, OAuthCredentialResolver):
            resolved: ProviderCredential | None = ProviderCredential(
                "bearer",
                await credential_resolver.resolve(**request),
            )
        else:
            resolved = credential_resolver(request)
            if inspect.isawaitable(resolved):
                resolved = await resolved
    except CredentialResolutionError:
        raise
    except Exception:
        raise CredentialResolutionError("credential_resolver_failed", provider_id) from None
    return _validate_credential(resolved, provider_id)


def redact_credential(credential: ProviderCredential) -> dict[str, CredentialKind]:
    """Return diagnostic-safe credential metadata without retaining its value."""
    _validate_credential(credential, "unknown")
    return {"type": credential.type}


def _validate_credential(
    credential: ProviderCredential | None,
    provider_id: str,
) -> ProviderCredential:
    if credential is None:
        raise CredentialResolutionError("credential_unavailable", provider_id)
    if (
        not isinstance(credential, ProviderCredential)
        or credential.type not in {"api_key", "bearer"}
        or not isinstance(credential.value, str)
        or not credential.value.strip()
    ):
        raise CredentialResolutionError("credential_invalid", provider_id)
    return credential


def _message(code: str, provider_id: str) -> str:
    messages = {
        "credential_unavailable": "Missing credential",
        "credential_invalid": "Invalid credential",
        "credential_refresh_failed": "Credential refresh failed",
        "credential_resolver_failed": "Credential resolver failed",
        "credential_oauth_scope_mismatch": "Credential scope does not satisfy",
        "credential_oauth_audience_mismatch": "Credential audience does not match",
        "credential_revoked": "Credential was revoked",
        "credential_auth_mode_unsupported": "Credential authentication mode is unsupported for",
    }
    return f"{messages.get(code, 'Credential resolution failed')} provider {provider_id}"
