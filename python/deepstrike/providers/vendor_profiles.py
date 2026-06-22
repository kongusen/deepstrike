"""Single source of truth for the Anthropic-compatible vendor backends (DeepSeek,
Kimi, Qwen, GLM, MiniMax) — parity with the Node SDK's ``vendor-profiles.ts``.

Each backend differs only by data — provider id, default model, base URL, and
per-model runtime policy — so the generic :class:`AnthropicCompatibleProvider`
reads a profile from here instead of every backend subclassing
``AnthropicProvider`` purely to carry configuration.

The per-model policy tables are also consumed by each backend's OpenAI-chat class
(same recommended ``max_turns``), so they are exported here to keep one
authoritative copy. This module imports nothing from the backend files, so there
is no import cycle.
"""
from __future__ import annotations

from dataclasses import dataclass

from .base import RuntimePolicy


@dataclass(frozen=True)
class AnthropicVendorProfile:
    provider_id: str
    default_model: str
    base_url: str
    policies: dict[str, RuntimePolicy]


DEEPSEEK_POLICIES: dict[str, RuntimePolicy] = {
    "deepseek-chat":     RuntimePolicy(max_turns=25),
    "deepseek-reasoner": RuntimePolicy(max_turns=50),
    "deepseek-r1":       RuntimePolicy(max_turns=50),
    "deepseek-v4-flash": RuntimePolicy(max_turns=20),
    "deepseek-v4-pro":   RuntimePolicy(max_turns=35),
}

KIMI_POLICIES: dict[str, RuntimePolicy] = {
    "moonshot-v1-8k":   RuntimePolicy(max_turns=15),
    "moonshot-v1-32k":  RuntimePolicy(max_turns=20),
    "moonshot-v1-128k": RuntimePolicy(max_turns=30),
    "kimi-k2.5":        RuntimePolicy(max_turns=30),
    "kimi-k2.6":        RuntimePolicy(max_turns=35),
    "kimi-k2-thinking": RuntimePolicy(max_turns=50),
    "kimi-k2-thinking-turbo": RuntimePolicy(max_turns=40),
}

QWEN_POLICIES: dict[str, RuntimePolicy] = {
    "qwen3.7-max-preview": RuntimePolicy(max_turns=45),
    "qwen3.7-plus-preview": RuntimePolicy(max_turns=40),
    "qwen3.6-max-preview": RuntimePolicy(max_turns=40),
    "qwen3.6-plus": RuntimePolicy(max_turns=35),
    "qwen3.6-flash": RuntimePolicy(max_turns=20),
    "qwen3.6-35b-a3b": RuntimePolicy(max_turns=25),
    "qwen3.6-27b": RuntimePolicy(max_turns=25),
    "qwen3.5-plus": RuntimePolicy(max_turns=35),
    "qwen3.5-flash": RuntimePolicy(max_turns=20),
    "qwen3.5-397b-a17b": RuntimePolicy(max_turns=35),
    "qwen3.5-122b-a10b": RuntimePolicy(max_turns=25),
    "qwen3.5-35b-a3b": RuntimePolicy(max_turns=20),
    "qwen3.5-27b": RuntimePolicy(max_turns=20),
}

GLM_POLICIES: dict[str, RuntimePolicy] = {
    "glm-5.1": RuntimePolicy(max_turns=50),
    "glm/glm-5.1": RuntimePolicy(max_turns=50),
    "glm-4-plus": RuntimePolicy(max_turns=35),
    "glm/glm-4-plus": RuntimePolicy(max_turns=35),
    "glm-4-flash": RuntimePolicy(max_turns=15),
    "glm/glm-4-flash": RuntimePolicy(max_turns=15),
    "glm-4-air": RuntimePolicy(max_turns=20),
    "glm/glm-4-air": RuntimePolicy(max_turns=20),
}

MINIMAX_POLICIES: dict[str, RuntimePolicy] = {
    "MiniMax-M2.7": RuntimePolicy(max_turns=35),
    "MiniMax-M2.7-highspeed": RuntimePolicy(max_turns=35),
    "MiniMax-M2.5": RuntimePolicy(max_turns=25),
    "MiniMax-M2.5-highspeed": RuntimePolicy(max_turns=25),
    "MiniMax-M2.1": RuntimePolicy(max_turns=25),
    "MiniMax-M2.1-highspeed": RuntimePolicy(max_turns=25),
    "MiniMax-M2": RuntimePolicy(max_turns=20),
    "MiniMax-Text-01": RuntimePolicy(max_turns=20),
}

ANTHROPIC_VENDOR_PROFILES: dict[str, AnthropicVendorProfile] = {
    "deepseek": AnthropicVendorProfile("deepseek", "deepseek-v4-flash", "https://api.deepseek.com/anthropic", DEEPSEEK_POLICIES),
    "kimi":     AnthropicVendorProfile("kimi", "kimi-k2.6", "https://api.moonshot.ai/anthropic", KIMI_POLICIES),
    "qwen":     AnthropicVendorProfile("qwen", "qwen3.6-plus", "https://dashscope-intl.aliyuncs.com/apps/anthropic", QWEN_POLICIES),
    "glm":      AnthropicVendorProfile("glm", "glm-5.1", "https://api.z.ai/api/anthropic", GLM_POLICIES),
    "minimax":  AnthropicVendorProfile("minimax", "MiniMax-M2.7", "https://api.minimaxi.com/anthropic", MINIMAX_POLICIES),
}
