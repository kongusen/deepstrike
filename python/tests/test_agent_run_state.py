import uuid

import pytest

from deepstrike import Agent
from deepstrike.providers.base import RenderedContext, ProviderRunState
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.tools.registry import tool


class StatefulTestProvider:
    def __init__(self) -> None:
        self.states: list[ProviderRunState | None] = []
        self._call_count = 0

    def create_run_state(self) -> ProviderRunState:
        return {"marker": str(uuid.uuid4())}

    async def complete(self, context: RenderedContext, tools, extensions=None):
        raise NotImplementedError

    async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
        self.states.append(state)
        self._call_count += 1
        if self._call_count == 1:
            yield ToolCallEvent(id="call_1", name="ping", arguments={})
            return
        yield TextDelta(delta="done")


@tool
def ping() -> str:
    """Ping."""
    return "pong"


@pytest.mark.asyncio
async def test_agent_threads_provider_run_state_through_turns():
    provider = StatefulTestProvider()
    agent = Agent(provider, max_tokens=2048, max_turns=4)
    agent.register(ping)

    async for _ in agent.run_streaming("Use ping once, then finish."):
        pass

    assert len(provider.states) == 2
    assert provider.states[0] is provider.states[1]
