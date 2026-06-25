"""Golden master for the Anthropic-compatible vendor providers (P1 refactor §6.1),
parity with the Node SDK's anthropic-compat-golden.test.ts. Locks
descriptor().provider/model + runtime_policy() across every known model so the
data-driven AnthropicCompatibleProvider refactor stays behavior-preserving.
Values transcribed from the pre-refactor vendor classes.
"""
from __future__ import annotations

import pytest

from deepstrike.providers.deepseek import DeepSeekAnthropicProvider
from deepstrike.providers.kimi import KimiAnthropicProvider
from deepstrike.providers.qwen import QwenAnthropicProvider
from deepstrike.providers.glm import GLMAnthropicProvider
from deepstrike.providers.minimax import MiniMaxAnthropicProvider

CASES = {
    "deepseek": (DeepSeekAnthropicProvider, "deepseek-v4-flash", {
        "deepseek-chat": 25, "deepseek-reasoner": 50, "deepseek-r1": 50,
        "deepseek-v4-flash": 20, "deepseek-v4-pro": 35,
    }),
    "kimi": (KimiAnthropicProvider, "kimi-k2.6", {
        "moonshot-v1-8k": 15, "moonshot-v1-32k": 20, "moonshot-v1-128k": 30,
        "kimi-k2.5": 30, "kimi-k2.6": 35, "kimi-k2-thinking": 50, "kimi-k2-thinking-turbo": 40,
    }),
    "qwen": (QwenAnthropicProvider, "qwen3.6-plus", {
        "qwen3.7-max-preview": 45, "qwen3.7-plus-preview": 40, "qwen3.6-max-preview": 40,
        "qwen3.6-plus": 35, "qwen3.6-flash": 20, "qwen3.6-35b-a3b": 25, "qwen3.6-27b": 25,
        "qwen3.5-plus": 35, "qwen3.5-flash": 20, "qwen3.5-397b-a17b": 35,
        "qwen3.5-122b-a10b": 25, "qwen3.5-35b-a3b": 20, "qwen3.5-27b": 20,
    }),
    "glm": (GLMAnthropicProvider, "glm-5.2", {
        "glm-5.2": 50, "glm/glm-5.2": 50,
        "glm-5.1": 50, "glm/glm-5.1": 50, "glm-4-plus": 35, "glm/glm-4-plus": 35,
        "glm-4-flash": 15, "glm/glm-4-flash": 15, "glm-4-air": 20, "glm/glm-4-air": 20,
    }),
    "minimax": (MiniMaxAnthropicProvider, "MiniMax-M3", {
        "MiniMax-M3": 35, "MiniMax-M3-highspeed": 35,
        "MiniMax-M2.7": 35, "MiniMax-M2.7-highspeed": 35, "MiniMax-M2.5": 25, "MiniMax-M2.5-highspeed": 25,
        "MiniMax-M2.1": 25, "MiniMax-M2.1-highspeed": 25, "MiniMax-M2": 20, "MiniMax-Text-01": 20,
    }),
}


@pytest.mark.parametrize("name", list(CASES))
def test_descriptor_default_model(name):
    cls, default_model, _ = CASES[name]
    d = cls("test-key").descriptor()
    assert d.provider == name
    assert d.protocol == "anthropic-messages"
    assert d.model == default_model


@pytest.mark.parametrize("name", list(CASES))
def test_descriptor_provider_for_arbitrary_model(name):
    cls, _, _ = CASES[name]
    d = cls("test-key", "some-custom-model").descriptor()
    assert d.provider == name
    assert d.model == "some-custom-model"


@pytest.mark.parametrize("name", list(CASES))
def test_runtime_policy_every_model(name):
    cls, _, policies = CASES[name]
    for model, max_turns in policies.items():
        assert cls("test-key", model).runtime_policy().max_turns == max_turns


@pytest.mark.parametrize("name", list(CASES))
def test_runtime_policy_unknown_model_is_empty(name):
    cls, _, _ = CASES[name]
    assert cls("test-key", "unknown-model-xyz").runtime_policy().max_turns is None
