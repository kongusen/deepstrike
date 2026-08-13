import pytest

from deepstrike.memory.protocols import MemoryQuery, MemoryRecord, MemoryScope
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime import RuntimeOptions, RuntimeRunner, collect_text
from deepstrike.tools.registry import tool


@pytest.mark.asyncio
async def test_semantic_page_out_commits_memory_summary():
  commit_calls = 0
  last_summary = ""

  class RecordingMemoryStore:
    async def put(self, agent_id: str, record: MemoryRecord):
      nonlocal commit_calls, last_summary
      commit_calls += 1
      last_summary = record.content

    async def get(self, agent_id: str, record_id: str):
      return None

    async def delete(self, agent_id: str, record_id: str):
      pass

    async def save_session(self, data):
      pass

    async def search(self, agent_id: str, query: MemoryQuery):
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

  async def memory_summarizer(archived, ctx):
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
    memory_scope=MemoryScope("agent-semantic-py", "semantic-page-out"),
        memory_store=RecordingMemoryStore(),
        memory_summarizer=memory_summarizer,
        baseline_tool_ids=["fill"],
      ))

  await collect_text(runner.run(session_id="semantic-page-out-py", goal="fill until compact"))

  assert commit_calls > 0
  assert "python long-term summary" in last_summary
