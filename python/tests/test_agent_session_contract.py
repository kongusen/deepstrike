from __future__ import annotations

from deepstrike import FileSessionLog, InMemorySessionLog, create_agent
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta


class Provider:
    async def complete(self, context: RenderedContext, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        yield TextDelta(delta="ok")


async def test_agent_session_reuses_host_session_log():
    log = InMemorySessionLog()
    agent = create_agent("session-agent", runtime_binding={"provider": Provider(), "session_log": log})
    session = agent.session("s-1")

    result = await session.run("hello")
    entries = await log.read("s-1")

    assert result == "ok"
    assert entries
    assert entries[0].event["kind"] == "run_started"


def test_agent_returns_one_handle_per_session_id():
    agent = create_agent("session-agent", runtime_binding={"provider": Provider()})

    assert agent.session("same") is agent.session("same")


def test_agent_can_create_a_durable_session_log_from_binding(tmp_path):
    agent = create_agent(
        "durable",
        runtime_binding={"provider": Provider(), "session_log_dir": str(tmp_path)},
    )

    assert isinstance(agent._session_log, FileSessionLog)
    assert hasattr(agent._session_log, "kernel_journal")
