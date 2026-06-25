from __future__ import annotations
from deepstrike._kernel import ToolSchema
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

    Models: glm-5.2 (default), glm-5.1, glm-4-plus, glm-4-flash, glm-4-air
    """

    def __init__(self, api_key: str, model: str = "glm-5.2", retry_config: RetryConfig | None = None, base_url: str = _GLM_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _GLM_POLICIES.get(self._model, RuntimePolicy())

    def _cache_key_params(self, context, tools) -> dict:
        # GLM caches automatically and does not document accepting OpenAI's prompt_cache_key
        # (accept-vs-400 unconfirmed) — safest to omit it.
        return {}

    # ── GLM web_search (vendor server tool; OpenAI-wire only) ──
    # Enable Zhipu's built-in web search by `extensions={"web_search": True}` (default config) or
    # `{"web_search": {...}}` (passthrough config: search_engine/search_pro, search_recency_filter,
    # search_domain_filter, search_result, count, …). Injected as a `{"type":"web_search",...}` entry
    # in the tools array (executed server-side); the selector is stripped from the wire params.

    def _wire_tools(self, tools: list[ToolSchema], extensions: dict | None = None) -> list[dict] | None:
        defs = list(super()._wire_tools(tools, extensions) or [])
        ws = (extensions or {}).get("web_search")
        if ws:
            defs.append({"type": "web_search", "web_search": ws if isinstance(ws, dict) else {}})
        return defs or None

    def _prepare_extensions(self, extensions: dict | None) -> dict:
        ext = extensions or {}
        if "web_search" not in ext:
            return dict(ext)
        return {k: v for k, v in ext.items() if k != "web_search"}
