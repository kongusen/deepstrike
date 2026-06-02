import asyncio

import pytest

from deepstrike.memory.protocols import CurationResult, CurationStats, MemoryEntry
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.tools.registry import tool


@pytest.mark.asyncio
async def test_semantic_page_out_commits_dream_summary():
  commit_calls = 0
  last_summary = ""

  class RecordingDreamStore:
    async def load_sessions(self, agent_id: str):
      return []

    async def load_memories(self, agent_id: str):
      return []

    async def commit(self, agent_id: str, result: CurationResult, existing):
      nonlocal commit_calls, last_summary
      commit_calls += 1
      last_summary = result.to_add[0].text if result.to_add else ""

    async def save_session(self, data):
      pass

    async def search(self, agent_id: str, query: str, top_k: int = 5):
      return []

  class FillProvider:
    def __init__(self) -> None:
      self.calls = 0

    async def complete(self, context, tools, extensions=None):
      raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
      self.calls += 1
      if self.calls <= 8:
        yield ToolCallEvent(id=f"c{self.calls}", name="fill", arguments={"n": self.calls})
        return
      yield TextDelta(delta="done")

  async def dream_summarizer(archived, ctx):
    return f"python long-term summary for {ctx.get('action', 'compress')}"

  @tool
  def fill(n: int = 1) -> str:
    """Fill context."""
    return "w" * 200

  from deepstrike.runtime import LocalExecutionPlane, InMemorySessionLog
  plane = LocalExecutionPlane()
  plane.register(fill)
  runner = RuntimeRunner(RuntimeOptions(
    provider=FillProvider(),
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=400,
    max_turns=20,
    agent_id="agent-semantic-py",
    dream_store=RecordingDreamStore(),
    dream_summarizer=dream_summarizer,
  ))

  await collect_text(runner.run(session_id="semantic-page-out-py", goal="fill until compact"))
  await asyncio.sleep(0.05)

  assert commit_calls > 0
  assert "python long-term summary" in last_summary
