"""Qwen multimodal: image input routes to the dashscope MultiModalConversation API (content = list of
modality dicts; response message.content also a list). Text-only stays on the Generation path. No live API.
"""
from __future__ import annotations

from http import HTTPStatus
from types import SimpleNamespace

import pytest

from deepstrike.providers.qwen import QwenProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message, ContentPartObj
from deepstrike.providers.stream import TextDelta, UsageEvent


def _img_ctx():
    return RenderedContext(turns=[Message(role="user", content="", content_parts=[
        ContentPartObj("text", text="what is this?"),
        ContentPartObj("image", data="BASE64", media_type="image/png"),
    ])])


class _FakeMM:
    def __init__(self, mode):
        self.mode = mode
        self.last = None

    async def call(self, **kwargs):
        self.last = kwargs
        if self.mode == "complete":
            msg = SimpleNamespace(content=[{"text": "a cat"}], tool_calls=[])
            return SimpleNamespace(
                status_code=HTTPStatus.OK,
                output=SimpleNamespace(choices=[SimpleNamespace(message=msg)]),
                usage=SimpleNamespace(total_tokens=7),
            )

        async def gen():
            yield SimpleNamespace(status_code=HTTPStatus.OK, output=SimpleNamespace(choices=[SimpleNamespace(message=SimpleNamespace(content=[{"text": "a "}]))]), usage=None)
            yield SimpleNamespace(status_code=HTTPStatus.OK, output=SimpleNamespace(choices=[SimpleNamespace(message=SimpleNamespace(content=[{"text": "cat"}]))]), usage=SimpleNamespace(input_tokens=10, output_tokens=2, total_tokens=12))
        return gen()


def _provider(mode):
    p = QwenProvider("k")
    p._mm_generation = _FakeMM(mode)
    return p


def test_has_image_input_detection():
    assert QwenProvider("k")._has_image_input(_img_ctx()) is True
    assert QwenProvider("k")._has_image_input(RenderedContext(turns=[Message(role="user", content="hi")])) is False


def test_build_mm_messages_format():
    user = QwenProvider("k")._build_mm_messages(_img_ctx())[-1]
    assert user["role"] == "user"
    assert {"text": "what is this?"} in user["content"]
    assert {"image": "data:image/png;base64,BASE64"} in user["content"]


@pytest.mark.asyncio
async def test_complete_routes_to_multimodal():
    p = _provider("complete")
    msg = await p.complete(_img_ctx(), [])
    assert msg.content == "a cat"  # extracted from the list-form content
    # the MultiModalConversation class was used, with list-form message content
    assert isinstance(p._mm_generation.last["messages"][-1]["content"], list)


@pytest.mark.asyncio
async def test_stream_routes_to_multimodal_with_usage():
    p = _provider("stream")
    events = [e async for e in p.stream(_img_ctx(), [])]
    assert "".join(e.delta for e in events if isinstance(e, TextDelta)) == "a cat"
    usage = [e for e in events if isinstance(e, UsageEvent)]
    assert usage and usage[0].total_tokens == 12
