"""Moonshot Context Caching (Kimi vendor signature feature, OpenAI-wire only). Verified-contract
characterization: create POSTs to /caching; a cache id/tag is referenced as a LEADING role:"cache"
message; the control selectors never leak to the wire. No live API (httpx mocked).
"""
from __future__ import annotations

import httpx
import pytest

from deepstrike.providers.kimi import KimiProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message

CTX = RenderedContext(turns=[Message(role="user", content="hi")])


class _FakeResp:
    def __init__(self, data):
        self._data = data

    def raise_for_status(self):
        pass

    def json(self):
        return self._data


class _FakeClient:
    def __init__(self, data, cap):
        self._data = data
        self._cap = cap

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        return False

    async def post(self, url, headers=None, json=None, timeout=None):
        self._cap["post"] = {"url": url, "headers": headers, "json": json}
        return _FakeResp(self._data)

    async def get(self, url, headers=None, timeout=None):
        self._cap["get"] = {"url": url, "headers": headers}
        return _FakeResp(self._data)


@pytest.mark.asyncio
async def test_create_context_cache_posts_to_caching(monkeypatch):
    cap: dict = {}
    data = {"id": "cache-x1", "object": "context_cache_object", "status": "pending", "tokens": 72}
    monkeypatch.setattr(httpx, "AsyncClient", lambda *a, **k: _FakeClient(data, cap))
    p = KimiProvider("KEY", model="moonshot-v1-128k")
    out = await p.create_context_cache([{"role": "system", "content": "big prompt"}], ttl=600, tags=["t1"], name="C")
    assert out["id"] == "cache-x1" and out["object"] == "context_cache_object"
    post = cap["post"]
    assert post["url"] == "https://api.moonshot.cn/v1/caching"
    assert post["headers"]["Authorization"] == "Bearer KEY"
    # create wants the model FAMILY (moonshot-v1), not the sized variant moonshot-v1-128k.
    assert post["json"] == {
        "model": "moonshot-v1",
        "messages": [{"role": "system", "content": "big prompt"}],
        "name": "C",
        "tags": ["t1"],
        "ttl": 600,
    }


@pytest.mark.asyncio
async def test_create_context_cache_explicit_model_overrides_family(monkeypatch):
    cap: dict = {}
    monkeypatch.setattr(httpx, "AsyncClient", lambda *a, **k: _FakeClient({"id": "cache-x"}, cap))
    p = KimiProvider("k", model="moonshot-v1-8k")
    await p.create_context_cache([{"role": "system", "content": "x"}], model="moonshot-v1")
    assert cap["post"]["json"]["model"] == "moonshot-v1"


@pytest.mark.asyncio
async def test_create_context_cache_expired_at_overrides_ttl(monkeypatch):
    cap: dict = {}
    monkeypatch.setattr(httpx, "AsyncClient", lambda *a, **k: _FakeClient({"id": "cache-x"}, cap))
    p = KimiProvider("k", model="moonshot-v1-8k")
    await p.create_context_cache([{"role": "system", "content": "x"}], expired_at=1893456000, ttl=600)
    assert cap["post"]["json"].get("expired_at") == 1893456000
    assert "ttl" not in cap["post"]["json"]


@pytest.mark.asyncio
async def test_resolve_cache_tag_gets_refs_endpoint(monkeypatch):
    cap: dict = {}
    monkeypatch.setattr(httpx, "AsyncClient", lambda *a, **k: _FakeClient({"tag": "mytag", "cache_id": "cache-x"}, cap))
    p = KimiProvider("KEY")
    out = await p.resolve_cache_tag("mytag")
    assert out == {"tag": "mytag", "cache_id": "cache-x"}
    assert cap["get"]["url"] == "https://api.moonshot.cn/v1/caching/refs/tags/mytag"


def test_cache_id_injects_leading_cache_message():
    p = KimiProvider("k")
    msgs = p._build_messages(CTX, {"context_cache_id": "cache-x", "context_cache_reset_ttl": 600})
    assert msgs[0] == {"role": "cache", "content": "cache_id=cache-x;reset_ttl=600"}
    assert msgs[1]["role"] == "user"


def test_cache_tag_form_without_reset_ttl():
    p = KimiProvider("k")
    msgs = p._build_messages(CTX, {"context_cache_tag": "mytag"})
    assert msgs[0] == {"role": "cache", "content": "tag=mytag"}


def test_no_cache_extension_no_cache_message():
    p = KimiProvider("k")
    msgs = p._build_messages(CTX, {})
    assert all(m.get("role") != "cache" for m in msgs)


def test_cache_selectors_stripped_from_wire_extensions():
    p = KimiProvider("k")
    prepared = p._prepare_extensions({"context_cache_id": "cache-x", "context_cache_reset_ttl": 600, "temperature": 0.5})
    assert prepared == {"temperature": 0.5}
