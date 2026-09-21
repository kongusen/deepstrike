"""The measured provider body stays bound to the subsequent dispatch."""
from copy import deepcopy
from types import SimpleNamespace

import pytest

from deepstrike._kernel import ContentPartObj, ModelMessage, ToolCall
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.openai_responses import OpenAIResponsesProvider
from deepstrike.providers.prepared_request import prepare_provider_request
from deepstrike.providers.request_plan import ProviderRequestEndpoint, create_provider_request_plan
from deepstrike.providers.stream import TextDelta


def fingerprint(prepared):
    return create_provider_request_plan(
        provider_id="test", model_id="model",
        endpoint=ProviderRequestEndpoint("test", "test", "https://example.test"),
        context={}, tools=[], execution=prepared.evidence(),
    ).fingerprint


@pytest.mark.asyncio
async def test_custom_counter_is_isolated_and_dispatch_restores_frozen_run_state():
    state = {"continuation": "before", "remove": True}
    context = RenderedContext(turns=[ModelMessage(role="user", content="before")])
    options = {"temperature": 0.2, "api_key": "credential"}
    seen = []

    class Provider:
        async def count_tokens(self, context, tools, extensions=None, state=None):
            context.turns.clear()
            tools.clear()
            extensions["temperature"] = 9
            state["continuation"] = "counter mutation"
            return SimpleNamespace(input_tokens=1)

        async def stream(self, context, tools, extensions=None, state=None):
            seen.append((context.turns[0].content, tools, dict(extensions), dict(state), state))
            del state["remove"]
            state["continuation"] = "after"
            yield TextDelta(delta="done")

    prepared = prepare_provider_request(Provider(), context, [{"name": "tool"}], options, state)
    before = fingerprint(prepared)
    assert prepared.scope == "adapter_input"
    assert "credential" not in str(prepared.evidence())
    await prepared.count_tokens()
    context.turns.clear()
    options["temperature"] = 8
    state["continuation"] = "external mutation"
    state["external"] = True
    assert [event.delta async for event in prepared.stream()] == ["done"]
    assert seen[0][:4] == ("before", [{"name": "tool"}],
                           {"temperature": 0.2, "api_key": "credential"},
                           {"continuation": "before", "remove": True})
    assert seen[0][4] is state
    assert state == {"continuation": "after"}
    assert fingerprint(prepared) == before


@pytest.mark.asyncio
async def test_anthropic_replay_is_frozen_once_for_count_and_dispatch(monkeypatch):
    provider = AnthropicProvider("test")
    calls = [ToolCall(id="call-1", name="lookup", arguments="{}")]
    context = RenderedContext(turns=[
        ModelMessage(role="user", content="question"),
        ModelMessage(role="assistant", content="answer", tool_calls=calls),
        ModelMessage(role="tool", content="", content_parts=[
            ContentPartObj("tool_result", call_id="call-1", output="result", is_error=False)]),
    ])
    replay = [{"type": "thinking", "thinking": "reason", "signature": "sig-a"},
              {"type": "text", "text": "answer"},
              {"type": "tool_use", "id": "call-1", "name": "lookup", "input": {}}]
    provider.seed_provider_replay("answer", calls, {"protocol": "anthropic-messages", "native_blocks": replay})
    prepared = provider.prepare_request(context, [], {"cacheBreakpointStrategy": "none"})
    original_fingerprint = fingerprint(prepared)
    count_bodies, stream_bodies = [], []

    async def count(**body):
        count_bodies.append(deepcopy(body))
        body["messages"].clear()  # A meter cannot mutate the subsequent generation body.
        return SimpleNamespace(input_tokens=12)

    class EmptyStream:
        async def __aenter__(self):
            return self
        async def __aexit__(self, *args):
            pass
        def __aiter__(self):
            return self
        async def __anext__(self):
            raise StopAsyncIteration

    def stream(**body):
        stream_bodies.append(body)
        return EmptyStream()

    provider._client.messages.count_tokens = count
    provider._client.messages.stream = stream
    replay[0]["signature"] = "sig-b"
    changed = provider.prepare_request(context, [], {"cacheBreakpointStrategy": "none"})
    assert fingerprint(changed) != original_fingerprint
    monkeypatch.setattr(provider, "_build_request_plan", lambda *args: pytest.fail("request re-encoded"))
    await prepared.count_tokens()
    _ = [event async for event in prepared.stream()]
    assert count_bodies[0]["messages"] == stream_bodies[0]["messages"]
    assert stream_bodies[0]["messages"][1]["content"][0]["signature"] == "sig-a"
    assert fingerprint(prepared) == original_fingerprint


@pytest.mark.asyncio
async def test_responses_continuation_is_frozen_for_native_count_and_stream(monkeypatch):
    provider = OpenAIResponsesProvider("test")
    state = {"previous_response_id": "resp-before", "covered_message_count": 2}
    context = RenderedContext(turns=[
        ModelMessage(role="user", content="covered"),
        ModelMessage(role="assistant", content="reply"),
        ModelMessage(role="user", content="tail"),
    ])
    prepared = provider.prepare_request(context, [], state=state)
    original_fingerprint = fingerprint(prepared)
    counted, streamed = [], []

    async def count(**body):
        counted.append(deepcopy(body))
        body["input"].clear()
        return SimpleNamespace(input_tokens=10)

    async def create(**body):
        streamed.append(body)
        async def events():
            yield SimpleNamespace(type="response.completed", response=SimpleNamespace(id="resp-after", usage=None))
        return events()

    provider._client = SimpleNamespace(responses=SimpleNamespace(
        create=create, input_tokens=SimpleNamespace(count=count)))
    state["previous_response_id"] = "external-change"
    state["covered_message_count"] = 0
    assert fingerprint(provider.prepare_request(context, [], state=state)) != original_fingerprint
    monkeypatch.setattr(provider._responses, "build_request", lambda *args: pytest.fail("request re-encoded"))
    await prepared.count_tokens()
    _ = [event async for event in prepared.stream()]
    assert counted[0]["input"] == streamed[0]["input"] == [{"role": "user", "content": "tail"}]
    assert counted[0]["previous_response_id"] == streamed[0]["previous_response_id"] == "resp-before"
    assert state["previous_response_id"] == "resp-after"
    assert fingerprint(prepared) == original_fingerprint


@pytest.mark.parametrize("state", [{"opaque": object()}, {1: "key"}, {"nan": float("nan")}, {"tuple": (1,)}])
def test_custom_opaque_state_requires_explicit_preparation(state):
    with pytest.raises(ValueError, match="implement prepare_request"):
        prepare_provider_request(object(), RenderedContext(), [], None, state)


def test_custom_cyclic_state_requires_explicit_preparation():
    state = {}
    state["cycle"] = state
    with pytest.raises(ValueError, match="implement prepare_request"):
        prepare_provider_request(object(), RenderedContext(), [], None, state)
