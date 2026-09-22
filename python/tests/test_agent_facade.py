import pytest
from deepstrike import create_agent, ModelMessage
from deepstrike.runtime.replay_provider import ReplayProvider
from deepstrike.runtime.session_log import InMemorySessionLog


@pytest.mark.asyncio
async def test_run_and_stream_share_host_session_binding():
    log = InMemorySessionLog()
    agent = create_agent("bound", binding={
        "provider": ReplayProvider([ModelMessage(role="assistant", content="first"), ModelMessage(role="assistant", content="second")]),
        "runtime_options": {"session_log": log},
    })
    result = await agent.run("first", session_id="same")
    assert result["output"] == "first"
    assert result["session_id"] == "same"
    assert result["status"] == "completed"
    events = [event async for event in agent.stream("second", session_id="same")]
    assert any(event.get("type") == "text_delta" and event.get("delta") == "second" for event in events)
    assert len([e for e in await log.read("same") if e.event["kind"] == "run_started"]) == 2
    assert "runtime_binding" not in agent.definition


def test_binding_cannot_be_embedded_in_definition():
    with pytest.raises(TypeError):
        create_agent("legacy", runtime_binding={})
