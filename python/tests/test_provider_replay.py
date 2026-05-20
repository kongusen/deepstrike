import pytest

from deepstrike._kernel import Message, ToolCall, ToolResult
from deepstrike.providers.anthropic import AnthropicProvider
from deepstrike.providers.base import RenderedContext, to_anthropic_messages
from deepstrike.providers.openai import OpenAIProvider
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import (
  FileSessionLog,
  LocalExecutionPlane,
  RuntimeOptions,
  RuntimeRunner,
  collect_text,
)
from deepstrike.tools.registry import tool


class CapturingAnthropicProvider(AnthropicProvider):
  def __init__(self) -> None:
    super().__init__("test-key")
    self.captured_messages: list[dict] | None = None

  async def stream(self, context, tools, extensions=None, state=None):
    self.captured_messages = self._build_messages(context.turns)
    yield TextDelta(delta="finished")


@pytest.mark.asyncio
async def test_wake_restores_thinking_blocks_from_provider_replay(tmp_path):
  session_id = "thinking-wake"
  await FileSessionLog(tmp_path).append(session_id, {
    "kind": "run_started", "run_id": "r1", "goal": "use ping", "criteria": [],
  })
  await FileSessionLog(tmp_path).append(session_id, {
    "kind": "llm_completed",
    "turn": 0,
    "content": "checking",
    "tool_calls": [ToolCall(id="call_ping", name="ping", arguments="{}")],
    "provider_replay": {
      "native_blocks": [
        {"type": "thinking", "thinking": "plan", "signature": "sig"},
        {"type": "text", "text": "checking"},
        {"type": "tool_use", "id": "call_ping", "name": "ping", "input": {}},
      ],
    },
  })
  await FileSessionLog(tmp_path).append(session_id, {
    "kind": "tool_completed",
    "turn": 0,
    "results": [ToolResult(call_id="call_ping", output="pong", is_error=False)],
  })

  @tool
  def ping() -> str:
    """Ping."""
    return "should-not-run"

  provider = CapturingAnthropicProvider()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=FileSessionLog(tmp_path),
    execution_plane=LocalExecutionPlane().register(ping),
    max_tokens=2048,
    max_turns=4,
  ))

  text = await collect_text(runner.wake(session_id))
  assert text == "finished"
  assert provider.captured_messages == [
    {"role": "user", "content": "use ping"},
    {
      "role": "assistant",
      "content": [
        {"type": "thinking", "thinking": "plan", "signature": "sig"},
        {"type": "text", "text": "checking"},
        {"type": "tool_use", "id": "call_ping", "name": "ping", "input": {}},
      ],
    },
    {
      "role": "user",
      "content": [{"type": "tool_result", "tool_use_id": "call_ping", "content": "pong", "is_error": False}],
    },
  ]


def test_anthropic_native_replay_hook():
  provider = AnthropicProvider("test-key")
  provider.seed_provider_replay("checking", [ToolCall("call_1", "lookup", '{"q":"x"}')], {
    "native_blocks": [
      {"type": "thinking", "thinking": "plan", "signature": "sig"},
      {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {"q": "x"}},
    ],
  })
  turns = [
    Message(role="user", content="hi"),
    Message(role="assistant", content="checking", tool_calls=[ToolCall("call_1", "lookup", '{"q":"x"}')]),
  ]
  replayed = to_anthropic_messages(
    turns,
    native_replay=lambda message: provider._native_assistant_blocks.get(provider._assistant_replay_key(message)),
  )
  assert replayed[1]["content"][0]["type"] == "thinking"


def test_openai_reasoning_replay_roundtrip():
  provider = OpenAIProvider("test-key")
  provider.seed_provider_replay("done", [ToolCall("call_1", "lookup", "{}")], {
    "reasoning_content": "thought",
  })
  context = RenderedContext(
    turns=[Message(role="assistant", content="done", tool_calls=[ToolCall("call_1", "lookup", "{}")])],
  )
  msgs = provider._build_messages(context)
  assert msgs[0]["reasoning_content"] == "thought"
  assert provider.peek_provider_replay("done", [ToolCall("call_1", "lookup", "{}")]) == {
    "reasoning_content": "thought",
  }


@pytest.mark.asyncio
async def test_file_session_log_provider_replay_roundtrip(tmp_path):
  session_log = FileSessionLog(tmp_path)
  await session_log.append("s1", {
    "kind": "llm_completed",
    "turn": 0,
    "content": "hi",
    "tool_calls": [],
    "provider_replay": {"reasoning_content": "trace"},
  })
  events = await session_log.read("s1")
  assert events[0].event["provider_replay"] == {"reasoning_content": "trace"}
