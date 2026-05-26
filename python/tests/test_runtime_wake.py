import pytest

from deepstrike._kernel import ToolCall, ToolResult
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime import (
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeOptions,
  RuntimeRunner,
  collect_text,
)
from deepstrike.tools.registry import tool


class ResumeAwareProvider:
  def __init__(self) -> None:
    self.stream_calls = 0

  async def complete(self, context: RenderedContext, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
    self.stream_calls += 1
    has_tool = any(m.role == "tool" for m in context.turns)
    if not has_tool:
      yield ToolCallEvent(id="call_ping", name="ping", arguments={})
      return
    yield TextDelta(delta="finished")


@tool
def ping() -> str:
  """Ping."""
  return "pong"


@pytest.mark.asyncio
async def test_wake_continues_after_tool_completed():
  session_log = InMemorySessionLog()
  session_id = "crash-test"
  await session_log.append(session_id, {
    "kind": "run_started", "run_id": "r1", "goal": "use ping", "criteria": [],
  })
  await session_log.append(session_id, {
    "kind": "llm_completed",
    "turn": 0,
    "content": "",
    "tool_calls": [ToolCall(id="call_ping", name="ping", arguments="{}")],
  })
  await session_log.append(session_id, {
    "kind": "tool_completed",
    "turn": 0,
    "results": [ToolResult(call_id="call_ping", output="pong", is_error=False)],
  })

  plane = LocalExecutionPlane()
  plane.register(ping)
  provider = ResumeAwareProvider()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=plane,
    max_tokens=2048,
    max_turns=4,
  ))

  text = await collect_text(runner.wake(session_id))
  assert text == "finished"
  assert provider.stream_calls == 1

  events = await session_log.read(session_id)
  assert any(e.event.get("kind") == "run_terminal" for e in events)


@pytest.mark.asyncio
async def test_run_session_continuity():
  class CapturingProvider:
    def __init__(self) -> None:
      self.calls: list[RenderedContext] = []

    async def complete(self, context, tools, extensions=None):
      raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
      self.calls.append(context)
      yield TextDelta(delta=f"answer-{len(self.calls)}")

  provider = CapturingProvider()
  session_log = InMemorySessionLog()
  plane = LocalExecutionPlane()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=session_log, execution_plane=plane, max_tokens=2048,
  ))
  sid = "chat-1"
  await collect_text(runner.run(session_id=sid, goal="My name is Ada."))
  await collect_text(runner.run(session_id=sid, goal="What is my name?"))

  assert any(m.content == "My name is Ada." for m in provider.calls[1].turns)
  assert any(m.content == "answer-1" for m in provider.calls[1].turns)
  assert any(m.content == "What is my name?" for m in provider.calls[1].turns)


@pytest.mark.asyncio
async def test_run_records_compressed_event():
  @tool
  def ping() -> str:
    """Return a large ping payload."""
    return "pong " * 200

  provider = ResumeAwareProvider()
  session_log = InMemorySessionLog()
  plane = LocalExecutionPlane()
  plane.register(ping)
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=plane,
    max_tokens=32,
    max_turns=4,
  ))

  await collect_text(runner.run(session_id="compressed-session", goal="use big_ping then finish"))

  events = await session_log.read("compressed-session")
  compressed = [e.event for e in events if e.event.get("kind") == "compressed"]
  assert compressed
  assert compressed[0]["archived_seq_range"][0] == 0


@pytest.mark.asyncio
async def test_run_reactive_compacts_and_retries_prompt_too_long():
  class TooLongThenOkProvider:
    def __init__(self) -> None:
      self.stream_calls = 0

    async def complete(self, context, tools, extensions=None):
      raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
      self.stream_calls += 1
      if self.stream_calls == 1:
        raise RuntimeError("413 prompt too long")
      yield TextDelta(delta="recovered")

  provider = TooLongThenOkProvider()
  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=LocalExecutionPlane(),
    max_tokens=1000,
    max_turns=4,
  ))

  text = await collect_text(runner.run(session_id="reactive-compact", goal="a" * 5000))

  assert text == "recovered"
  assert provider.stream_calls == 2
  events = await session_log.read("reactive-compact")
  assert any(e.event.get("kind") == "compressed" for e in events)


@pytest.mark.asyncio
async def test_context_rollback_on_tool_failure_and_replay_consistency():
  class FakeProvider:
    def __init__(self) -> None:
      self.stream_calls = 0

    async def complete(self, context, tools, extensions=None):
      raise NotImplementedError

    async def stream(self, context, tools, extensions=None, state=None):
      self.stream_calls += 1
      if self.stream_calls == 1:
        yield ToolCallEvent(id="call_1", name="fail_tool", arguments={})
        return
      yield TextDelta(delta="Recovered")

  @tool
  def fail_tool() -> str:
    """Fails always."""
    raise ValueError("Tool crashed!")

  provider = FakeProvider()
  session_log = InMemorySessionLog()
  plane = LocalExecutionPlane()
  plane.register(fail_tool)

  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=plane,
    max_turns=4,
    max_tokens=1000,
  ))

  session_id = "test-rollback"
  text = await collect_text(runner.run(session_id=session_id, goal="run"))
  assert text == "Recovered"

  # 1. Verify rollbacked event was recorded.
  events = await session_log.read(session_id)
  rollbacked = any(e.event.get("kind") == "rollbacked" for e in events)
  assert rollbacked

  # 2. Verify replay/recovery logic truncates the messages history appropriately.
  from deepstrike.runtime.runner import _replay_messages
  msgs = _replay_messages(events)
  assert len(msgs) == 2
  assert msgs[0].role == "user"
  assert msgs[1].role == "assistant"
  assert msgs[1].content == "Recovered"
