from __future__ import annotations

import pytest

from deepstrike import FallbackProvider, TextDelta


class Failing:
    async def complete(self, context, tools, extensions=None):
        raise RuntimeError("offline")

    async def stream(self, context, tools, extensions=None, state=None):
        raise RuntimeError("offline")
        yield


class Working:
    async def complete(self, context, tools, extensions=None):
        return {"role": "assistant", "content": "ok"}

    async def stream(self, context, tools, extensions=None, state=None):
        yield TextDelta(delta="ok")


async def test_fallback_provider_tries_next_provider_before_output():
    provider = FallbackProvider([Failing(), Working()])
    events = [event async for event in provider.stream({}, [])]
    assert events[0].delta == "ok"


async def test_fallback_provider_does_not_switch_after_output():
    class Partial:
        async def stream(self, context, tools, extensions=None, state=None):
            yield TextDelta(delta="partial")
            raise RuntimeError("broken")

    provider = FallbackProvider([Partial(), Working()])
    with pytest.raises(RuntimeError, match="broken"):
        _ = [event async for event in provider.stream({}, [])]
