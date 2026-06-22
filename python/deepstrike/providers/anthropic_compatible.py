"""Generic provider for vendors exposing an Anthropic-compatible Messages
endpoint (DeepSeek, Kimi, Qwen, GLM, MiniMax, …) — parity with the Node SDK's
``anthropic-compatible.ts``.

All wire behavior is inherited from :class:`AnthropicProvider`; the only
per-vendor variation is configuration, supplied as an
:class:`AnthropicVendorProfile`. This replaces the family of near-identical
``<Vendor>AnthropicProvider`` subclasses that existed only to carry that config.
"""
from __future__ import annotations

from .anthropic import AnthropicProvider
from .base import RetryConfig, RuntimePolicy
from .vendor_profiles import AnthropicVendorProfile


class AnthropicCompatibleProvider(AnthropicProvider):
    def __init__(
        self,
        profile: AnthropicVendorProfile,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(
            api_key,
            model or profile.default_model,
            retry_config,
            base_url=base_url or profile.base_url,
        )
        self._vendor_profile = profile

    def _provider_name(self) -> str:
        return self._vendor_profile.provider_id

    def runtime_policy(self) -> RuntimePolicy:
        return self._vendor_profile.policies.get(self._model, RuntimePolicy())
