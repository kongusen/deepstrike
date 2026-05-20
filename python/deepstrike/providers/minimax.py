from __future__ import annotations

from .anthropic import AnthropicProvider
from .base import RetryConfig, RuntimePolicy

_MINIMAX_ANTHROPIC_BASE = "https://api.minimaxi.com/anthropic"

_MINIMAX_POLICIES: dict[str, RuntimePolicy] = {
    "MiniMax-M2.7":    RuntimePolicy(max_turns=35),
    "MiniMax-M2.5":    RuntimePolicy(max_turns=25),
    "MiniMax-Text-01": RuntimePolicy(max_turns=20),
}


class MiniMaxProvider(AnthropicProvider):
    """MiniMax M2.x via the Anthropic-compatible endpoint (interleaved thinking + tool use)."""

    def __init__(
        self,
        api_key: str,
        model: str = "MiniMax-M2.7",
        retry_config: RetryConfig | None = None,
        base_url: str = _MINIMAX_ANTHROPIC_BASE,
    ):
        super().__init__(api_key, model, retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _MINIMAX_POLICIES.get(self._model, RuntimePolicy())
