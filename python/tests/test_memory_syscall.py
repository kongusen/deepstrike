import pytest

from deepstrike.memory.protocols import (
  MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope,
)
from deepstrike.providers.stream import TextDelta
from deepstrike.runtime import RuntimeOptions, RuntimeRunner, InMemorySessionLog, LocalExecutionPlane


class Provider:
  async def complete(self, context, tools, extensions=None):
    raise NotImplementedError

  async def stream(self, context, tools, extensions=None, state=None):
    yield TextDelta(delta="")

SCOPE = MemoryScope("agent-memory", "python-runtime")
def memory(name: str, content: str) -> MemoryRecord:
  return MemoryRecord(
    record_id=f"record-{name or 'invalid'}", scope=SCOPE, name=name, kind="feedback", content=content,
    description="User prefers small focused tests",
    provenance=MemoryProvenance(author="host", trust="host_verified"),
    created_at=1, updated_at=1, confidence=0.9,
  )


@pytest.mark.asyncio
async def test_write_memory_commits_to_dream_store_after_kernel_validation():
  committed = None

  class Store:
    async def upsert(self, agent_id: str, record: MemoryRecord):
      nonlocal committed
      committed = record

    async def save_session(self, data):
      pass

    async def search(self, agent_id: str, query: MemoryQuery):
      return []

  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=session_log,
    execution_plane=LocalExecutionPlane(),
    max_tokens=1024,
    agent_id="agent-memory",
    dream_store=Store(),
  ))

  await runner.write_memory(memory("prefers-small-tests", "User prefers focused unit tests for SDK behavior."), session_id="memory-syscall-py")

  assert committed.content == "User prefers focused unit tests for SDK behavior."
  assert committed.name == "prefers-small-tests"
  events = await session_log.read("memory-syscall-py")
  assert any(e.event["kind"] == "memory_written" for e in events)


@pytest.mark.asyncio
async def test_query_memory_returns_dream_store_hits_after_kernel_observation():
  hit = MemoryRecall(record=memory("testing", "Use small focused tests."), score=0.9, why="fixture")

  class Store:
    async def upsert(self, agent_id: str, record):
      pass

    async def save_session(self, data):
      pass

    async def search(self, agent_id: str, query: MemoryQuery):
      return [hit] if "tests" in query.query and query.top_k == 1 else []

  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=session_log,
    execution_plane=LocalExecutionPlane(),
    max_tokens=1024,
    agent_id="agent-memory",
    dream_store=Store(),
  ))

  hits = await runner.query_memory(MemoryQuery(SCOPE, "Need memory about tests", top_k=1), session_id="memory-query-syscall-py")

  assert hits == [hit]
  events = await session_log.read("memory-query-syscall-py")
  assert any(e.event["kind"] == "memory_queried" for e in events)
  assert any(e.event["kind"] == "memory_retrieval_result" for e in events)


@pytest.mark.asyncio
async def test_write_memory_logs_validation_failure_without_commit():
  committed = False

  class Store:
    async def upsert(self, agent_id: str, record):
      nonlocal committed
      committed = True

    async def save_session(self, data):
      pass

    async def search(self, agent_id: str, query: MemoryQuery):
      return []

  session_log = InMemorySessionLog()
  runner = RuntimeRunner(RuntimeOptions(
    provider=Provider(),
    session_log=session_log,
    execution_plane=LocalExecutionPlane(),
    max_tokens=1024,
    agent_id="agent-memory",
    dream_store=Store(),
  ))

  invalid = memory("", "invalid write")
  invalid.description = "missing name"
  await runner.write_memory(invalid, session_id="memory-validation-fail-py")

  assert committed is False
  events = await session_log.read("memory-validation-fail-py")
  assert any(e.event["kind"] == "memory_validation_failed" for e in events)
  assert not any(e.event["kind"] == "memory_written" for e in events)
