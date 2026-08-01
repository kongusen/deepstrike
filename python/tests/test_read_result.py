"""Canonical external payload and read_result integration coverage."""

import asyncio

import pytest

from deepstrike import InMemorySessionLog, LocalExecutionPlane, RuntimeOptions, RuntimeRunner
from deepstrike.providers.base import RenderedContext
from deepstrike.providers.stream import TextDelta, ToolCallEvent
from deepstrike.runtime.payload_store import PayloadStore
from deepstrike.tools.registry import tool


class ExternalThenReadProvider:
  def __init__(self) -> None:
    self.calls: list[RenderedContext] = []
    self.seen_tools: list[list] = []

  async def complete(self, context, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context: RenderedContext, tools, extensions=None, state=None):
    self.calls.append(context)
    self.seen_tools.append(list(tools))
    if len(self.calls) == 1:
      yield ToolCallEvent(id="big-1", name="big_out", arguments={})
      return
    if any(t.name == "read_result" for t in tools):
      yield ToolCallEvent(id="read-1", name="read_result", arguments={"call_id": "big-1"})
      return
    yield TextDelta(delta="done")


@pytest.mark.asyncio
async def test_read_result_loads_an_external_payload(tmp_path):
  huge = "y" * (100 * 1024)
  payload_store = PayloadStore(storage_dir=str(tmp_path / "payloads"))
  provider = ExternalThenReadProvider()

  @tool
  def big_out() -> str:
    """Return an oversized result."""
    return huge

  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=LocalExecutionPlane().register(big_out),
    max_tokens=128_000,
    max_turns=8,
    payload_store=payload_store,
  ))

  async for _ in runner.run(goal="fetch big output", session_id="read-result-run"):
    pass

  assert not any(t.name == "read_result" for t in provider.seen_tools[0])
  assert any(any(t.name == "read_result" for t in tools) for tools in provider.seen_tools)
  assert len(provider.calls) >= 3
  assert huge[:4000] in repr(provider.calls[2])


@pytest.mark.asyncio
async def test_payload_store_hashes_opaque_locators_and_commits_atomically(tmp_path):
  storage_dir = tmp_path / "payloads"
  store = PayloadStore(storage_dir=str(storage_dir))
  locator = "../../outside/../payload"

  await asyncio.gather(*[
    store.persist_payload("session", locator, "stable-output") for _ in range(8)
  ])

  files = list(storage_dir.iterdir())
  assert len(files) == 1
  assert ".." not in files[0].name
  assert not list(storage_dir.glob("*.tmp"))
  assert await store.load_payload("session", locator) == "stable-output"
  assert store._active_writes == {}


@pytest.mark.asyncio
async def test_payload_store_is_session_scoped(tmp_path):
  store = PayloadStore(storage_dir=str(tmp_path / "payloads"))
  await store.persist_payload("session-a", "payload:1", "alpha")
  await store.persist_payload("session-b", "payload:1", "beta")

  assert await store.load_payload("session-a", "payload:1") == "alpha"
  assert await store.load_payload("session-b", "payload:1") == "beta"
  assert await store.load_payload("session-c", "payload:1") is None
