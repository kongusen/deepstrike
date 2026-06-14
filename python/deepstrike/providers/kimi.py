from __future__ import annotations
from .anthropic import AnthropicProvider
from .base import RetryConfig, RuntimePolicy
from .openai import OpenAIProvider

_KIMI_ANTHROPIC_BASE = "https://api.moonshot.ai/anthropic"
_MOONSHOT_BASE_URL = "https://api.moonshot.cn/v1"

_KIMI_POLICIES: dict[str, RuntimePolicy] = {
    "moonshot-v1-8k":   RuntimePolicy(max_turns=15),
    "moonshot-v1-32k":  RuntimePolicy(max_turns=20),
    "moonshot-v1-128k": RuntimePolicy(max_turns=30),
    "kimi-k2.5":        RuntimePolicy(max_turns=30),
    "kimi-k2.6":        RuntimePolicy(max_turns=35),
    "kimi-k2-thinking": RuntimePolicy(max_turns=50),
    "kimi-k2-thinking-turbo": RuntimePolicy(max_turns=40),
}


class KimiAnthropicProvider(AnthropicProvider):
    """Kimi (Moonshot AI) over its Anthropic-compatible endpoint."""

    def __init__(
        self,
        api_key: str,
        model: str = "kimi-k2.6",
        retry_config: RetryConfig | None = None,
        base_url: str = _KIMI_ANTHROPIC_BASE,
    ):
        super().__init__(api_key, model, retry_config, base_url=base_url)

    def _provider_name(self) -> str:
        return "kimi"

    def runtime_policy(self) -> RuntimePolicy:
        return _KIMI_POLICIES.get(self._model, RuntimePolicy())


class KimiProvider(OpenAIProvider):
    """Kimi (Moonshot AI) provider — OpenAI-compatible.

    Models: kimi-k2.6, kimi-k2.5, kimi-k2-thinking, kimi-k2-thinking-turbo, moonshot-v1-*
    """

    def __init__(self, api_key: str, model: str = "moonshot-v1-8k", retry_config: RetryConfig | None = None, base_url: str = _MOONSHOT_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _KIMI_POLICIES.get(self._model, RuntimePolicy())
