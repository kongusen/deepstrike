from __future__ import annotations
from .base import RetryConfig, RuntimePolicy
from .openai import OpenAIProvider

_MOONSHOT_BASE_URL = "https://api.moonshot.cn/v1"

_KIMI_POLICIES: dict[str, RuntimePolicy] = {
    "moonshot-v1-8k":   RuntimePolicy(max_turns=15),
    "moonshot-v1-32k":  RuntimePolicy(max_turns=20),
    "moonshot-v1-128k": RuntimePolicy(max_turns=30),
    "kimi-k2.5":        RuntimePolicy(max_turns=30),
    "kimi-k2.6":        RuntimePolicy(max_turns=35),
}


class KimiProvider(OpenAIProvider):
    """Kimi (Moonshot AI) provider — OpenAI-compatible.

    Models: moonshot-v1-8k, moonshot-v1-32k, moonshot-v1-128k
    Vision: moonshot-v1-8k-vision-preview (image URL only)
    """

    def __init__(self, api_key: str, model: str = "moonshot-v1-8k", retry_config: RetryConfig | None = None, base_url: str = _MOONSHOT_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _KIMI_POLICIES.get(self._model, RuntimePolicy())
