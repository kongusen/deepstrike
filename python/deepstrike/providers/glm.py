from __future__ import annotations
from .base import RetryConfig, RuntimePolicy
from .openai import OpenAIProvider

_GLM_BASE_URL = "https://open.bigmodel.cn/api/paas/v4"

_GLM_POLICIES: dict[str, RuntimePolicy] = {
    "glm-5.1": RuntimePolicy(max_turns=50),
    "glm/glm-5.1": RuntimePolicy(max_turns=50),
    "glm-4-plus": RuntimePolicy(max_turns=35),
    "glm/glm-4-plus": RuntimePolicy(max_turns=35),
    "glm-4-flash": RuntimePolicy(max_turns=15),
    "glm/glm-4-flash": RuntimePolicy(max_turns=15),
    "glm-4-air": RuntimePolicy(max_turns=20),
    "glm/glm-4-air": RuntimePolicy(max_turns=20),
}


class GLMProvider(OpenAIProvider):
    """GLM (Zhipu AI) provider — OpenAI-compatible.

    Models: glm-5.1, glm-4-plus, glm-4-flash, glm-4-air
    """

    def __init__(self, api_key: str, model: str = "glm-5.1", retry_config: RetryConfig | None = None, base_url: str = _GLM_BASE_URL):
        super().__init__(api_key=api_key, model=model, retry_config=retry_config, base_url=base_url)

    def runtime_policy(self) -> RuntimePolicy:
        return _GLM_POLICIES.get(self._model, RuntimePolicy())
