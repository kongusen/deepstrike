"""Provider protocol vocabulary shared by every Python provider surface."""
from typing import Literal

GenerationProtocol = Literal[
    "anthropic-messages",
    "openai-chat",
    "openai-responses",
    "gemini",
    "ollama-chat",
]

GENERATION_PROTOCOLS: tuple[GenerationProtocol, ...] = (
    "anthropic-messages",
    "openai-chat",
    "openai-responses",
    "gemini",
    "ollama-chat",
)
