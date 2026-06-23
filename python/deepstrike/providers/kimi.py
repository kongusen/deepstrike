from __future__ import annotations
from .base import RetryConfig, RuntimePolicy
from .openai import OpenAIProvider
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import KIMI_POLICIES as _KIMI_POLICIES, ANTHROPIC_VENDOR_PROFILES

_MOONSHOT_BASE_URL = "https://api.moonshot.cn/v1"


class KimiAnthropicProvider(AnthropicCompatibleProvider):
    """Kimi (Moonshot AI) over its Anthropic-compatible endpoint.

    Deprecated: prefer ``kimi(protocol="anthropic")``. Data-driven via
    ``ANTHROPIC_VENDOR_PROFILES["kimi"]``; thin shim for backward compat / isinstance.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["kimi"], api_key, model, retry_config, base_url)


class KimiProvider(OpenAIProvider):
    """Kimi (Moonshot AI) provider — OpenAI-compatible.

    Models: kimi-k2.6, kimi-k2.5, kimi-k2-thinking, kimi-k2-thinking-turbo, moonshot-v1-*
    """

    def __init__(self, api_key: str, model: str = "moonshot-v1-8k", retry_config: RetryConfig | None = None, base_url: str = _MOONSHOT_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _KIMI_POLICIES.get(self._model, RuntimePolicy())

    def _cache_key_params(self, context, tools) -> dict:
        # Moonshot caches automatically and does not document accepting OpenAI's prompt_cache_key
        # (accept-vs-400 unconfirmed) — safest to omit it. Use the OpenAI-side Context Caching API for
        # explicit caching instead (TODO: surface it as a first-class feature).
        return {}
