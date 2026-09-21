"""P-07 contracts for data-driven provider construction and OpenAI-chat dialects."""
from __future__ import annotations

import pytest

from deepstrike._kernel import ModelMessage, ToolSchema
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.factories import deepseek, gemini, glm, kimi, minimax, qwen
from deepstrike.providers.openai import OpenAIProvider
from deepstrike.providers.runtime_registry import (
    OPENAI_CHAT_DIALECTS,
    create_provider,
    resolve_runtime_profile,
)
from deepstrike.providers.model_registry import model_registry


def _tool() -> ToolSchema:
    return ToolSchema(name="lookup", description="Lookup", parameters='{"type":"object"}')


def test_factories_construct_from_registry_and_attach_the_resolved_dialect() -> None:
    providers = {
        "deepseek": deepseek(api_key="key", model="deepseek-v4-pro"),
        "kimi": kimi(api_key="key"),
        "glm": glm(api_key="key"),
        "minimax": minimax(api_key="key", protocol="openai"),
        "qwen": qwen(api_key="key"),
    }

    for provider_id, provider in providers.items():
        assert provider._wire_dialect is OPENAI_CHAT_DIALECTS[provider_id]

    assert providers["deepseek"]._model == "deepseek-v4-pro"
    assert providers["kimi"]._model == "kimi-k2.6"
    assert providers["glm"]._model == "glm-5.2"
    assert providers["minimax"]._model == "MiniMax-M3"
    assert providers["qwen"]._model == "qwen3.6-plus"
    assert deepseek(api_key="key")._model == "deepseek-chat"


def test_openai_chat_dialect_drives_descriptor_request_hooks_and_replay() -> None:
    dialect = OPENAI_CHAT_DIALECTS["deepseek"]
    provider = OpenAIProvider("key", model="deepseek-v4-pro", dialect=dialect)

    assert provider.descriptor().provider == "deepseek"
    assert provider.descriptor().reasoning == {
        "supported": True,
        "preserve_across_tool_turns": True,
        "requires_replay_for_tool_turns": True,
    }
    assert provider._prepare_extensions({"thinking": False, "reasoningEffort": "max"}) == {
        "reasoning_effort": "max",
        "extra_body": {"thinking": {"type": "disabled"}},
    }
    assert provider._cache_key_params(None, []) == {}
    assert provider._uses_inline_thinking_tags() is False
    assert provider._expose_reasoning_delta({"expose_reasoning": True}) is True

    provider._remember_complete_replay(
        "answer",
        [],
        type("Reasoning", (), {"reasoning_content": "plan", "reasoning_details": None, "native_tool_calls": []})(),
    )
    assert provider._replay_fields
    assert next(iter(provider._replay_fields.values()))["provider"] == "deepseek"


def test_glm_dialect_owns_server_tool_and_wire_extension_filtering() -> None:
    provider = glm(api_key="key")
    assert provider._prepare_extensions({"web_search": {"count": 3}, "temperature": 0.2}) == {"temperature": 0.2}
    assert provider._wire_tools([_tool()], {"web_search": {"count": 3}}) == [
        {"type": "function", "function": {"name": "lookup", "description": "Lookup", "parameters": {"type": "object"}}},
        {"type": "web_search", "web_search": {"count": 3}},
    ]


def test_runtime_profile_resolves_region_endpoint_and_dialect_from_tables() -> None:
    runtime, endpoint, dialect = resolve_runtime_profile(
        "kimi", model="kimi-k2.6", protocol="openai", region="global"
    )
    assert runtime.endpoint_id == "kimi.global.openai"
    assert endpoint.base_url == "https://api.moonshot.ai/v1"
    assert dialect is OPENAI_CHAT_DIALECTS["kimi"]


def test_runtime_profile_uses_the_same_dialect_default_model_as_the_factory() -> None:
    runtime, _, _ = resolve_runtime_profile("glm", protocol="openai")
    assert runtime.model_id == glm(api_key="key")._model == "glm-5.2"


def test_factory_attaches_runtime_for_gemini_before_its_sdk_model_is_initialized() -> None:
    provider = gemini(api_key="key")

    assert provider._resolved_runtime.provider_id == "gemini"
    assert provider._resolved_runtime.model_id == provider._model_name == "gemini-2.0-flash"


def test_kimi_cache_helper_remains_available_from_table_constructed_provider() -> None:
    provider = create_provider("kimi", api_key="key", protocol="openai")
    messages = provider._build_messages(
        RenderedContext(turns=[ModelMessage(role="user", content="hello")]),
        {"context_cache_id": "cache-1"},
    )
    assert messages[0] == {"role": "cache", "content": "cache_id=cache-1"}


@pytest.mark.parametrize(
    ("provider_id", "endpoint_id", "protocol"),
    [
        ("deepseek", "deepseek.openai", "openai-chat"),
        ("kimi", "kimi.cn.openai", "openai-chat"),
        ("qwen", "qwen.cn.openai", "openai-chat"),
        ("glm", "glm.cn.openai", "openai-chat"),
        ("minimax", "minimax.anthropic", "anthropic-messages"),
    ],
)
def test_spc_020_default_routing_is_consistent_across_registries(
    provider_id: str,
    endpoint_id: str,
    protocol: str,
) -> None:
    registration = model_registry.resolve(f"{provider_id}/fixture-model")
    runtime, _, _ = resolve_runtime_profile(provider_id, model="fixture-model", protocol=None)

    assert registration is not None
    assert registration.default_endpoint_id == endpoint_id
    assert runtime.endpoint_id == endpoint_id
    assert runtime.protocol == protocol


@pytest.mark.parametrize(
    ("provider_id", "protocol"),
    [
        ("deepseek", "openai-chat"),
        ("kimi", "openai-chat"),
        ("qwen", "openai-chat"),
        ("glm", "openai-chat"),
        ("minimax", "anthropic-messages"),
    ],
)
def test_spc_020_direct_factory_uses_provider_default_protocol(provider_id: str, protocol: str) -> None:
    provider = create_provider(provider_id, api_key="key", model="fixture-model", protocol=None)
    assert provider._resolved_runtime.protocol == protocol


def test_spc_020_explicit_compatible_protocols_remain_available() -> None:
    assert create_provider(
        "deepseek", api_key="key", model="fixture-model", protocol="anthropic"
    ).descriptor().protocol == "anthropic-messages"
    assert create_provider(
        "minimax", api_key="key", model="fixture-model", protocol="openai"
    ).descriptor().protocol == "openai-chat"


def test_spc_021_custom_base_url_does_not_inherit_cache_evidence() -> None:
    provider = create_provider(
        "deepseek",
        api_key="key",
        model="deepseek-chat",
        protocol=None,
        base_url="https://proxy.invalid/v1",
    )
    assert provider._resolved_runtime.effective_capabilities.prompt_caching.state == "unknown"
