"""Golden master for the canonical Anthropic-wire backend factories."""
from __future__ import annotations

import pytest

from deepstrike.providers.factories import deepseek, glm, kimi, minimax

CASES = {
    "deepseek": (deepseek, "deepseek-v4-flash", {
        "deepseek-chat": 25, "deepseek-reasoner": 50, "deepseek-r1": 50,
        "deepseek-v4-flash": 20, "deepseek-v4-pro": 35,
    }),
    "kimi": (kimi, "kimi-k2.6", {
        "moonshot-v1-8k": 15, "moonshot-v1-32k": 20, "moonshot-v1-128k": 30,
        "kimi-k2.5": 30, "kimi-k2.6": 35, "kimi-k2-thinking": 50, "kimi-k2-thinking-turbo": 40,
    }),
    "glm": (glm, "glm-5.2", {
        "glm-5.2": 50, "glm/glm-5.2": 50,
        "glm-5.1": 50, "glm/glm-5.1": 50, "glm-4-plus": 35, "glm/glm-4-plus": 35,
        "glm-4-flash": 15, "glm/glm-4-flash": 15, "glm-4-air": 20, "glm/glm-4-air": 20,
    }),
    "minimax": (minimax, "MiniMax-M3", {
        "MiniMax-M3": 35, "MiniMax-M3-highspeed": 35,
        "MiniMax-M2.7": 35, "MiniMax-M2.7-highspeed": 35, "MiniMax-M2.5": 25, "MiniMax-M2.5-highspeed": 25,
        "MiniMax-M2.1": 25, "MiniMax-M2.1-highspeed": 25, "MiniMax-M2": 20, "MiniMax-Text-01": 20,
    }),
}


@pytest.mark.parametrize("name", list(CASES))
def test_descriptor_default_model(name):
    factory, default_model, _ = CASES[name]
    d = factory(api_key="test-key", protocol="anthropic").descriptor()
    assert d.provider == name
    assert d.protocol == "anthropic-messages"
    assert d.model == default_model


@pytest.mark.parametrize("name", list(CASES))
def test_descriptor_provider_for_arbitrary_model(name):
    factory, _, _ = CASES[name]
    d = factory(api_key="test-key", model="some-custom-model", protocol="anthropic").descriptor()
    assert d.provider == name
    assert d.model == "some-custom-model"


@pytest.mark.parametrize("name", list(CASES))
def test_runtime_policy_every_model(name):
    factory, _, policies = CASES[name]
    for model, max_turns in policies.items():
        assert factory(api_key="test-key", model=model, protocol="anthropic").runtime_policy().max_turns == max_turns


@pytest.mark.parametrize("name", list(CASES))
def test_runtime_policy_unknown_model_is_empty(name):
    factory, _, _ = CASES[name]
    assert factory(api_key="test-key", model="unknown-model-xyz", protocol="anthropic").runtime_policy().max_turns is None
