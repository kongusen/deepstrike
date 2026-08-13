from types import SimpleNamespace

import pytest

from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner, collect_text


class MeasuredProvider:
  def __init__(self, tokens: int = 12) -> None:
    self.tokens = tokens
    self.count_calls = 0
    self.stream_calls = 0

  def descriptor(self):
    return SimpleNamespace(provider="test", protocol="openai-chat", model="fixture")

  async def count_tokens(self, context: RenderedContext, tools, extensions=None):
    self.count_calls += 1
    return SimpleNamespace(
      input_tokens=self.tokens,
      source={"kind": "native", "provider": "test"},
      confidence="exact",
    )

  async def complete(self, context: RenderedContext, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
    self.stream_calls += 1
    yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_records_native_measurement_before_provider_execution():
  provider = MeasuredProvider()
  log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider, session_log=log, execution_plane=LocalExecutionPlane(), max_tokens=256,
  ))

  events = [event async for event in runner.run(session_id="measurement-native", goal="hello")]
  assert not [event for event in events if type(event).__name__ == "ErrorEvent"]
  assert provider.count_calls == 1
  assert provider.stream_calls == 1
  measured = [entry.event for entry in await log.read("measurement-native") if entry.event["kind"] == "prompt_measured"]
  assert len(measured) == 1
  assert measured[0]["measurement"]["source"] == {"kind": "native", "provider": "test"}


@pytest.mark.asyncio
async def test_measured_overflow_does_not_call_provider():
  provider = MeasuredProvider(tokens=128)
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=64,
  ))

  assert await collect_text(runner.run(session_id="measurement-overflow", goal="hello")) == ""
  assert provider.count_calls == 1
  assert provider.stream_calls == 0


@pytest.mark.asyncio
async def test_false_context_overflow_still_measures_and_calls_provider(monkeypatch):
  provider = MeasuredProvider()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=LocalExecutionPlane(),
    max_tokens=256,
  ))

  from deepstrike.runtime import runner as runner_module
  original = runner_module.RuntimeRunner._with_structured_tool_outputs

  def with_false_overflow(self, context, overlay):
    rendered = original(self, context, overlay)
    rendered.budget_overflow = False
    return rendered

  monkeypatch.setattr(runner_module.RuntimeRunner, "_with_structured_tool_outputs", with_false_overflow)
  assert await collect_text(runner.run(session_id="measurement-false-overflow", goal="hello")) == "done"
  assert provider.count_calls == 1
  assert provider.stream_calls == 1
