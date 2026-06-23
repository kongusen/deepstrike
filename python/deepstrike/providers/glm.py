from __future__ import annotations
from .base import RetryConfig, RuntimePolicy
from .openai import OpenAIProvider
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import GLM_POLICIES as _GLM_POLICIES, ANTHROPIC_VENDOR_PROFILES

_GLM_BASE_URL = "https://open.bigmodel.cn/api/paas/v4"


class GLMAnthropicProvider(AnthropicCompatibleProvider):
    """GLM (Zhipu AI) over its Anthropic-compatible endpoint.

    Deprecated: prefer ``glm(protocol="anthropic")``. Data-driven via
    ``ANTHROPIC_VENDOR_PROFILES["glm"]``; thin shim for backward compat / isinstance.
    """

    def __init__(
        self,
        api_key: str,
        model: str | None = None,
        retry_config: RetryConfig | None = None,
        base_url: str | None = None,
    ):
        super().__init__(ANTHROPIC_VENDOR_PROFILES["glm"], api_key, model, retry_config, base_url)


class GLMProvider(OpenAIProvider):
    """GLM (Zhipu AI) provider — OpenAI-compatible.

    Models: glm-5.1, glm-4-plus, glm-4-flash, glm-4-air
    """

    def __init__(self, api_key: str, model: str = "glm-5.1", retry_config: RetryConfig | None = None, base_url: str = _GLM_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _GLM_POLICIES.get(self._model, RuntimePolicy())

    def _cache_key_params(self, context, tools) -> dict:
        # GLM caches automatically and does not document accepting OpenAI's prompt_cache_key
        # (accept-vs-400 unconfirmed) — safest to omit it. GLM's web_search tool lives on this
        # OpenAI-side wire (TODO: surface it as a first-class feature).
        return {}
