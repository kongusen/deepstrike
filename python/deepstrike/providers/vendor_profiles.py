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

# (vendor, region, protocol) -> base URL. The CN vendors serve BOTH mainland and international users
# on BOTH an OpenAI-compatible and an Anthropic-compatible wire; region and protocol are independent
# axes. Region-specific: a region also has its own API key (caller supplies the matching key) — region
# selection rebinds the endpoint, not the credential. Cells that don't exist are simply absent
# (verified 2026): Qwen has NO mainland Anthropic endpoint (DashScope's Anthropic wire is Singapore-
# only), so mainland Qwen must use the OpenAI-compatible wire. Vendor signature features
# (Moonshot Context Caching, GLM web_search, Qwen enable_search/multimodal) live ONLY on the
# OpenAI/native wire — never on the Anthropic wire — so neither protocol can be dropped.
VENDOR_ENDPOINTS: dict[tuple[str, str, str], str] = {
    ("kimi", "cn",     "openai"):    "https://api.moonshot.cn/v1",
    ("kimi", "cn",     "anthropic"): "https://api.moonshot.cn/anthropic",
    ("kimi", "global", "openai"):    "https://api.moonshot.ai/v1",
    ("kimi", "global", "anthropic"): "https://api.moonshot.ai/anthropic",
    ("glm",  "cn",     "openai"):    "https://open.bigmodel.cn/api/paas/v4",
    ("glm",  "cn",     "anthropic"): "https://open.bigmodel.cn/api/anthropic",
    ("glm",  "global", "openai"):    "https://api.z.ai/api/paas/v4",
    ("glm",  "global", "anthropic"): "https://api.z.ai/api/anthropic",
    ("qwen", "cn",     "openai"):    "https://dashscope.aliyuncs.com/compatible-mode/v1",
    ("qwen", "global", "openai"):    "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
    ("qwen", "global", "anthropic"): "https://dashscope-intl.aliyuncs.com/apps/anthropic",
    # ("qwen","cn","anthropic") intentionally absent — Singapore/international-only.
}


def resolve_vendor_endpoint(vendor: str, region: str, protocol: str) -> str:
    """Base URL for a (vendor, region, protocol) cell. Raises with the available cells if the
    combination does not exist (e.g. mainland Qwen over the Anthropic wire)."""
    url = VENDOR_ENDPOINTS.get((vendor, region, protocol))
    if url is None:
        available = sorted({(r, p) for (v, r, p) in VENDOR_ENDPOINTS if v == vendor})
        raise ValueError(
            f"No {vendor!r} endpoint for region={region!r} protocol={protocol!r}. "
            f"Available (region, protocol) for {vendor!r}: {available}. "
            f"Note: each region needs its own API key."
        )
    return url


ANTHROPIC_VENDOR_PROFILES: dict[str, AnthropicVendorProfile] = {
    "deepseek": AnthropicVendorProfile("deepseek", "deepseek-v4-flash", "https://api.deepseek.com/anthropic", DEEPSEEK_POLICIES),
    "kimi":     AnthropicVendorProfile("kimi", "kimi-k2.6", "https://api.moonshot.ai/anthropic", KIMI_POLICIES),
    "qwen":     AnthropicVendorProfile("qwen", "qwen3.6-plus", "https://dashscope-intl.aliyuncs.com/apps/anthropic", QWEN_POLICIES),
    "glm":      AnthropicVendorProfile("glm", "glm-5.1", "https://api.z.ai/api/anthropic", GLM_POLICIES),
    "minimax":  AnthropicVendorProfile("minimax", "MiniMax-M2.7", "https://api.minimaxi.com/anthropic", MINIMAX_POLICIES),
}
