"""Region as a first-class endpoint dimension for the CN vendors (Kimi/GLM/Qwen) + the
prompt_cache_key omission on Kimi/GLM openai-chat. Both regions × both protocols must be reachable;
vendor signature features live on the openai/native wire, so neither protocol is dropped.
"""
from __future__ import annotations

import pytest

from deepstrike.providers.vendor_profiles import resolve_vendor_endpoint, VENDOR_ENDPOINTS
from deepstrike.providers.factories import kimi, glm
from deepstrike.providers.kimi import KimiProvider, KimiAnthropicProvider
from deepstrike.providers.glm import GLMProvider, GLMAnthropicProvider


def test_resolver_returns_all_four_cells_for_kimi_and_glm():
    assert resolve_vendor_endpoint("kimi", "cn", "openai") == "https://api.moonshot.cn/v1"
    assert resolve_vendor_endpoint("kimi", "cn", "anthropic") == "https://api.moonshot.cn/anthropic"
    assert resolve_vendor_endpoint("kimi", "global", "openai") == "https://api.moonshot.ai/v1"
    assert resolve_vendor_endpoint("kimi", "global", "anthropic") == "https://api.moonshot.ai/anthropic"
    assert resolve_vendor_endpoint("glm", "cn", "openai") == "https://open.bigmodel.cn/api/paas/v4"
    assert resolve_vendor_endpoint("glm", "global", "anthropic") == "https://api.z.ai/api/anthropic"


def test_resolver_raises_on_missing_qwen_mainland_anthropic():
    # DashScope's Anthropic wire is Singapore/international-only.
    assert ("qwen", "cn", "anthropic") not in VENDOR_ENDPOINTS
    with pytest.raises(ValueError, match="No 'qwen' endpoint for region='cn' protocol='anthropic'"):
        resolve_vendor_endpoint("qwen", "cn", "anthropic")
    # but the openai wire exists for mainland Qwen
    assert resolve_vendor_endpoint("qwen", "cn", "openai") == "https://dashscope.aliyuncs.com/compatible-mode/v1"


def test_kimi_factory_region_selects_endpoint_for_each_protocol():
    # global openai (was only reachable by hand-typing a base_url before)
    p = kimi(api_key="k", region="global", protocol="openai")
    assert isinstance(p, KimiProvider) and p._base_url == "https://api.moonshot.ai/v1"
    # mainland anthropic (the previously-unreachable cell)
    p2 = kimi(api_key="k", region="cn", protocol="anthropic")
    assert isinstance(p2, KimiAnthropicProvider) and "api.moonshot.cn/anthropic" in str(p2._client.base_url)


def test_glm_factory_region_selects_endpoint():
    p = glm(api_key="k", region="cn", protocol="openai")
    assert isinstance(p, GLMProvider) and p._base_url == "https://open.bigmodel.cn/api/paas/v4"
    p2 = glm(api_key="k", region="global", protocol="anthropic")
    assert isinstance(p2, GLMAnthropicProvider) and "z.ai/api/anthropic" in str(p2._client.base_url)


def test_explicit_base_url_overrides_region():
    p = kimi(api_key="k", region="global", base_url="https://gw.example.com/v1")
    assert p._base_url == "https://gw.example.com/v1"


def test_region_omitted_keeps_existing_defaults():
    # backward compatible: no region → existing per-class default base URLs
    assert kimi(api_key="k")._base_url == "https://api.moonshot.cn/v1"
    assert glm(api_key="k")._base_url == "https://open.bigmodel.cn/api/paas/v4"


def test_kimi_glm_openai_omit_prompt_cache_key():
    # No CN vendor documents accepting prompt_cache_key (worst case 400) — omit it.
    assert KimiProvider("k")._cache_key_params(None, []) == {}
    assert GLMProvider("k")._cache_key_params(None, []) == {}
