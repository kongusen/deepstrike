import pytest

from deepstrike._kernel import ToolCall, ToolResult
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent, UsageEvent
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
  # goal lands in system_text (old kernel), state_turn (new kernel), or turns[0] (legacy)
  ctx = provider.calls[1]
  all_text = [ctx.system_text] + [m.content for m in ctx.turns]
  if getattr(ctx, "state_turn", None) is not None:
      all_text.append(ctx.state_turn.content)
  assert any("What is my name?" in t for t in all_text)


@pytest.mark.asyncio
async def test_run_records_compressed_event():
  @tool
  def ping() -> str:
    """Return a large ping payload."""
    return "pong " * 200

  class PressureProvider(ResumeAwareProvider):
    async def stream(self, context, tools, extensions=None, state=None):
      yield UsageEvent(total_tokens=940, input_tokens=940)
      async for event in super().stream(context, tools, extensions, state):
        yield event

  provider = PressureProvider()
  session_log = InMemorySessionLog()
  plane = LocalExecutionPlane()
  plane.register(ping)
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=plane,
    max_tokens=1024,
    max_turns=4,
  ))

  session_id = "compressed-session"
  await collect_text(runner.run(session_id=session_id, goal="use big_ping then finish"))
  # A single live ContextUnit is protected. A second user turn creates a legal boundary at which
  # the completed tool transaction can be archived.
  await collect_text(runner.run(session_id=session_id, goal="continue"))

  events = await session_log.read(session_id)
  compressed = [e.event for e in events if e.event.get("kind") == "compressed"]
  assert compressed, [entry.event for entry in events]
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

  session_id = "reactive-compact"
  # Keep more completed ContextUnits than the protected recent-unit floor so a reactive 413 has a
  # legal whole-unit eviction candidate.
  for index in range(4):
    await session_log.append(session_id, {
      "kind": "run_started",
      "run_id": f"seed-{index}",
      "goal": (f"seed-{index} " * 90),
      "criteria": [],
    })
    await session_log.append(session_id, {
      "kind": "llm_completed",
      "turn": index,
      "content": (f"prior-{index} " * 60),
      "tool_calls": [],
    })
    await session_log.append(session_id, {
      "kind": "run_terminal",
      "reason": "completed",
      "turns_used": 1,
      "total_tokens": 0,
    })

  text = await collect_text(runner.run(session_id=session_id, goal="continue"))

  events = await session_log.read(session_id)
  assert text == "recovered", (provider.stream_calls, [entry.event for entry in events])
  assert provider.stream_calls == 2
  assert any(e.event.get("kind") == "compressed" for e in events)


@pytest.mark.asyncio
async def test_recoverable_tool_failure_preserves_replay_context():
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

  events = await session_log.read(session_id)
  assert not any(e.event.get("kind") == "rollbacked" for e in events)

  from deepstrike.runtime.runner import _replay_messages
  msgs = _replay_messages(events)
  assert len(msgs) == 4
  assert msgs[0].role == "user"
  assert msgs[1].role == "assistant"
  assert msgs[1].tool_calls[0].name == "fail_tool"
  assert msgs[2].role == "tool"
  assert msgs[2].content_parts[0].type == "tool_result"
  assert msgs[2].content_parts[0].call_id == "call_1"
  assert msgs[2].content_parts[0].is_error is True
  assert msgs[3].role == "assistant"
  assert msgs[3].content == "Recovered"
