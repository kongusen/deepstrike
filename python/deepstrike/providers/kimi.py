from __future__ import annotations
import httpx
from deepstrike._kernel import ToolSchema
from .base import RenderedContext, RetryConfig, RuntimePolicy
from .openai import OpenAIProvider
from .anthropic_compatible import AnthropicCompatibleProvider
from .vendor_profiles import KIMI_POLICIES as _KIMI_POLICIES, ANTHROPIC_VENDOR_PROFILES

_MOONSHOT_BASE_URL = "https://api.moonshot.cn/v1"
# Caller knobs that select a Moonshot Context Cache; expressed as the leading role:"cache" message,
# never forwarded as request params.
_CACHE_CONTROL_KEYS = ("context_cache_id", "context_cache_tag", "context_cache_reset_ttl")


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

    def __init__(self, api_key: str, model: str = "kimi-k2.6", retry_config: RetryConfig | None = None, base_url: str = _MOONSHOT_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _KIMI_POLICIES.get(self._model, RuntimePolicy())

    def _cache_key_params(self, context, tools) -> dict:
        # Moonshot caches automatically and does not document accepting OpenAI's prompt_cache_key
        # (accept-vs-400 unconfirmed) — safest to omit it. Explicit caching is the Context Caching API
        # below.
        return {}

    # ── Moonshot Context Caching (vendor signature feature; OpenAI-wire only) ──
    # Explicit cache: create a cache object once, then reference its id/tag on later calls to skip
    # re-billing the cached prefix. Only the moonshot-v1-* models support it (kimi-k2* auto-cache).
    # Verified contract (2026): POST {base}/caching → {"id":"cache-…","object":"context_cache_object",
    # "status","tokens","expired_at"}; reference via a leading {"role":"cache","content":
    # "cache_id=…;reset_ttl=…"} (or "tag=…;reset_ttl=…") message. Endpoint exists on both .cn and .ai.

    async def create_context_cache(
        self,
        messages: list[dict],
        *,
        model: str | None = None,
        tools: list[dict] | None = None,
        ttl: int | None = 3600,
        expired_at: int | None = None,
        name: str | None = None,
        tags: list[str] | None = None,
    ) -> dict:
        """Create a Moonshot Context Cache and return the cache object (incl. ``id``). Reference the
        returned id on later complete()/stream() via ``extensions={"context_cache_id": <id>}`` (or a
        ``context_cache_tag``), optionally ``context_cache_reset_ttl``. ``ttl`` is seconds (relative);
        pass ``expired_at`` (epoch seconds) instead for an absolute expiry.

        Explicit caching is a moonshot-v1 feature, and the create call wants the model FAMILY
        (``moonshot-v1``), not a sized variant — `moonshot-v1-8k` 400s with "model family is invalid".
        We derive the family from the provider model; pass ``model`` to override."""
        body: dict = {"model": model or self._cache_model_family(), "messages": messages}
        if tools is not None:
            body["tools"] = tools
        if name is not None:
            body["name"] = name
        if tags is not None:
            body["tags"] = tags
        if expired_at is not None:
            body["expired_at"] = expired_at
        elif ttl is not None:
            body["ttl"] = ttl
        async with httpx.AsyncClient() as client:
            resp = await client.post(
                f"{self._base_url}/caching",
                headers={"Authorization": f"Bearer {self._client.api_key}", "Content-Type": "application/json"},
                json=body,
                timeout=30,
            )
            resp.raise_for_status()
            return resp.json()

    def _cache_model_family(self) -> str:
        # The /caching create endpoint wants the model family. Only moonshot-v1 supports explicit
        # caching (kimi-k2* auto-cache); sized variants like moonshot-v1-8k are rejected.
        return "moonshot-v1" if self._model.startswith("moonshot-v1") else self._model

    async def resolve_cache_tag(self, tag: str) -> dict:
        """Resolve a cache tag to its current cache object (``{"tag","cache_id"}``). GET
        {base}/caching/refs/tags/{tag}."""
        async with httpx.AsyncClient() as client:
            resp = await client.get(
                f"{self._base_url}/caching/refs/tags/{tag}",
                headers={"Authorization": f"Bearer {self._client.api_key}"},
                timeout=30,
            )
            resp.raise_for_status()
            return resp.json()

    def _prepare_extensions(self, extensions: dict | None) -> dict:
        # The context-cache selectors become the leading role:"cache" message (see _build_messages);
        # strip them from the wire so they're not echoed as unknown request params.
        ext = extensions or {}
        if not any(k in ext for k in _CACHE_CONTROL_KEYS):
            return dict(ext)
        return {k: v for k, v in ext.items() if k not in _CACHE_CONTROL_KEYS}

    def _build_messages(self, context: RenderedContext, extensions: dict | None = None) -> list[dict]:
        msgs = super()._build_messages(context, extensions)
        cache_msg = self._context_cache_message(extensions)
        return [cache_msg, *msgs] if cache_msg is not None else msgs

    def _context_cache_message(self, extensions: dict | None) -> dict | None:
        ext = extensions or {}
        cache_id = ext.get("context_cache_id")
        tag = ext.get("context_cache_tag")
        if cache_id:
            ref = f"cache_id={cache_id}"
        elif tag:
            ref = f"tag={tag}"
        else:
            return None
        reset_ttl = ext.get("context_cache_reset_ttl")
        if reset_ttl is not None:
            ref += f";reset_ttl={int(reset_ttl)}"
        return {"role": "cache", "content": ref}
