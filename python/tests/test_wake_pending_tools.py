"""Wake after llm_completed with pending tools (no tool_completed yet)."""

from __future__ import annotations

import pytest

from deepstrike._kernel import ToolCall
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime import (
  FileSessionLog,
  LocalExecutionPlane,
  RuntimeOptions,
  RuntimeRunner,
  collect_text,
)
from deepstrike.tools.registry import tool

ping_runs = {"n": 0}


@tool
def ping() -> str:
  """Ping."""
  ping_runs["n"] += 1
  return "pong"


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


@pytest.mark.asyncio
async def test_wake_executes_pending_tools_after_llm_completed(tmp_path):
  """SessionLog-only pending tools cannot drive wake execution (Node wake-recovery parity)."""
  ping_runs["n"] = 0
  session_id = "pending-tools"
  session_log = FileSessionLog(str(tmp_path))

  await session_log.append(session_id, {
    "kind": "run_started", "run_id": "r1", "goal": "use ping", "criteria": [],
  })
  await session_log.append(session_id, {
    "kind": "llm_completed",
    "turn": 0,
    "content": "checking",
    "tool_calls": [ToolCall(id="call_ping", name="ping", arguments="{}")],
  })

  provider = ResumeAwareProvider()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=LocalExecutionPlane().register(ping),
    max_tokens=2048,
    max_turns=4,
  ))

  with pytest.raises(RuntimeError, match="restored canonical operation has no pending effect or terminal"):
    await collect_text(runner.wake(session_id))
  assert ping_runs["n"] == 0
  assert provider.stream_calls == 0
