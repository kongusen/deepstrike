"""G1 parity: the Python OpenAI Responses provider continues with previous_response_id and only
sends the uncovered tail (mirror of node/tests/openai-responses-provider.test.ts)."""
from types import SimpleNamespace
import pytest

from deepstrike.providers.openai_responses import OpenAIResponsesProvider
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
from deepstrike._kernel import ContentPartObj, Message, ToolCall


def _ev(type_, **kw):
    return SimpleNamespace(type=type_, **kw)


@pytest.mark.asyncio
async def test_continues_with_previous_response_id_and_sends_only_the_uncovered_tail():
    provider = OpenAIResponsesProvider("test-key")
    state = provider.create_run_state()
    requests: list[dict] = []
    call_count = 0

    class FakeResponses:
        async def create(self, **kwargs):
            nonlocal call_count
            requests.append(kwargs)
            call_count += 1
            current = call_count

            async def gen():
                if current == 1:
                    item = SimpleNamespace(type="function_call", call_id="call_1", name="lookup", arguments="")
                    yield _ev("response.output_item.added", output_index=0, item=item)
                    yield _ev("response.function_call_arguments.done", output_index=0, arguments='{"city":"Shanghai"}')
                    done_item = SimpleNamespace(type="function_call", call_id="call_1", name="lookup", arguments='{"city":"Shanghai"}')
                    yield _ev("response.output_item.done", output_index=0, item=done_item)
                    yield _ev("response.completed", response=SimpleNamespace(
                        id="resp_1", usage=SimpleNamespace(total_tokens=12, input_tokens=10, output_tokens=2, input_tokens_details=None)))
                    return
                yield _ev("response.output_text.delta", delta="done")
                yield _ev("response.completed", response=SimpleNamespace(
                    id="resp_2", usage=SimpleNamespace(total_tokens=20, input_tokens=15, output_tokens=5, input_tokens_details=None)))

            return gen()

    provider._client = SimpleNamespace(responses=FakeResponses())

    first_ctx = RenderedContext(system_text="system rules", turns=[Message(role="user", content="Find weather")])
    first_events = [e async for e in provider.stream(first_ctx, [], None, state)]

    second_ctx = RenderedContext(system_text="system rules", turns=[
        Message(role="user", content="Find weather"),
        Message(role="assistant", content="", tool_calls=[ToolCall(id="call_1", name="lookup", arguments='{"city":"Shanghai"}')]),
        Message(role="tool", content="", content_parts=[ContentPartObj("tool_result", call_id="call_1", output="sunny", is_error=False)]),
    ])
    second_events = [e async for e in provider.stream(second_ctx, [], None, state)]

    # First turn: cold start, full input, no previous_response_id; captures resp_1.
    assert any(isinstance(e, ToolCallEvent) and e.name == "lookup" and e.arguments == {"city": "Shanghai"} for e in first_events)
    assert any(isinstance(e, UsageEvent) and e.total_tokens == 12 for e in first_events)
    assert "previous_response_id" not in requests[0]
    assert requests[0]["input"] == [{"role": "user", "content": "Find weather"}]
    assert requests[0]["instructions"] == "system rules"

    # Second turn: continuation — only the uncovered tail (the tool result) + previous_response_id.
    assert requests[1]["previous_response_id"] == "resp_1"
    assert requests[1]["input"] == [{"type": "function_call_output", "call_id": "call_1", "output": "sunny"}]
    assert any(isinstance(e, TextDelta) and e.delta == "done" for e in second_events)

    # State advanced to resp_2 for the next turn.
    assert state["previous_response_id"] == "resp_2"


@pytest.mark.asyncio
async def test_degrades_to_full_resend_when_no_previous_response_id():
    """No chain id (cold/expired/resume) ⇒ the full history is sent (stateless default)."""
    provider = OpenAIResponsesProvider("test-key")
    requests: list[dict] = []

    class FakeResponses:
        async def create(self, **kwargs):
            requests.append(kwargs)

            async def gen():
                yield _ev("response.output_text.delta", delta="ok")
                yield _ev("response.completed", response=SimpleNamespace(id="resp_x", usage=None))

            return gen()

    provider._client = SimpleNamespace(responses=FakeResponses())

    ctx = RenderedContext(turns=[
        Message(role="user", content="a"),
        Message(role="assistant", content="b"),
        Message(role="user", content="c"),
    ])
    # state with covered_message_count but no previous_response_id ⇒ full history.
    _ = [e async for e in provider.stream(ctx, [], None, {"covered_message_count": 2})]

    assert "previous_response_id" not in requests[0]
    assert requests[0]["input"] == [
        {"role": "user", "content": "a"},
        {"role": "assistant", "content": "b"},
        {"role": "user", "content": "c"},
    ]
