"""Qwen DashScope web search (vendor feature) made first-class: enable_search + search_options reach
the DashScope call on BOTH complete and stream (it was silently dropped on the stream path before).
No live API — the dashscope generation client is faked.
"""
from __future__ import annotations

from http import HTTPStatus
from types import SimpleNamespace

import pytest

from deepstrike.providers.qwen import QwenProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message

CTX = RenderedContext(turns=[Message(role="user", content="latest qwen news?")])


class _FakeGen:
    """Stand-in for dashscope AioGeneration. complete() expects a response object; stream() expects an
    async iterable of chunks. `mode` selects which."""

    def __init__(self, mode):
        self.mode = mode
        self.last_kwargs = None

    async def call(self, **kwargs):
        self.last_kwargs = kwargs
        if self.mode == "complete":
            msg = SimpleNamespace(content="ok", tool_calls=[])
            return SimpleNamespace(
                status_code=HTTPStatus.OK,
                output=SimpleNamespace(choices=[SimpleNamespace(message=msg)]),
                usage=SimpleNamespace(total_tokens=5),
            )

        async def gen():
            yield SimpleNamespace(status_code=HTTPStatus.OK, output=SimpleNamespace(choices=[]), usage=None)
        return gen()


def _provider(mode):
    p = QwenProvider("k")
    p._generation = _FakeGen(mode)
    return p


@pytest.mark.asyncio
async def test_stream_forwards_enable_search_and_options():
    p = _provider("stream")
    opts = {"search_strategy": "pro"}
    _ = [e async for e in p.stream(CTX, [], {"enable_search": True, "search_options": opts})]
    assert p._generation.last_kwargs.get("enable_search") is True
    assert p._generation.last_kwargs.get("search_options") == opts


@pytest.mark.asyncio
async def test_stream_no_search_by_default():
    p = _provider("stream")
    _ = [e async for e in p.stream(CTX, [], {})]
    assert "enable_search" not in p._generation.last_kwargs


@pytest.mark.asyncio
async def test_complete_forwards_enable_search():
    p = _provider("complete")
    await p.complete(CTX, [], {"enable_search": True})
    assert p._generation.last_kwargs.get("enable_search") is True
