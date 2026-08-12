import pytest

from deepstrike.memory import DurableMemory, InMemoryMemoryStore, Memory, MemoryProvenance, MemoryRecord, MemoryScope, MemoryStore


SCOPE = MemoryScope("tenant-test", "research")


def record(record_id: str, content: str) -> MemoryRecord:
  return MemoryRecord(
    record_id=record_id,
    scope=SCOPE,
    name=record_id,
    kind="project",
    content=content,
    description=content,
    provenance=MemoryProvenance(author="host", trust="host_verified"),
    created_at=1,
    updated_at=1,
  )


@pytest.mark.asyncio
async def test_durable_memory_exposes_search_get_put_delete_without_exposing_memory_store_protocol() -> None:
  store: MemoryStore = InMemoryMemoryStore()
  memory: Memory = DurableMemory(store, "agent-a", SCOPE)

  await memory.put(record("architecture", "kernel architecture notes"))
  assert [value.record_id for value in await memory.search("architecture", top_k=1)] == ["architecture"]
  assert (await memory.get("architecture")).content == "kernel architecture notes"

  await memory.delete("architecture")
  assert await memory.get("architecture") is None
  assert await memory.search("architecture") == []


@pytest.mark.asyncio
async def test_durable_memory_enforces_its_bound_scope() -> None:
  store: MemoryStore = InMemoryMemoryStore()
  memory: Memory = DurableMemory(store, "agent-a", SCOPE)
  foreign = MemoryRecord(
    **{**record("foreign", "private note").__dict__, "scope": MemoryScope("tenant-test", "private")}
  )

  with pytest.raises(ValueError, match="scope"):
    await memory.put(foreign)
  await store.put("agent-a", foreign)
  assert await memory.get("foreign") is None
  await memory.delete("foreign")
  assert (await store.get("agent-a", "foreign")).record_id == "foreign"


@pytest.mark.asyncio
async def test_durable_memory_filters_cross_scope_host_search_results() -> None:
  foreign = MemoryRecord(
    **{**record("foreign", "private note").__dict__, "scope": MemoryScope("tenant-test", "private")}
  )

  class LeakyStore:
    async def put(self, agent_id: str, value: MemoryRecord) -> None: pass
    async def get(self, agent_id: str, record_id: str) -> MemoryRecord | None: return None
    async def delete(self, agent_id: str, record_id: str) -> None: pass
    async def search(self, agent_id: str, query: object):
      from deepstrike.memory import MemoryRecall
      return [MemoryRecall(record=foreign, score=1, why="broken host store")]
    async def save_session(self, data: object) -> None: pass

  memory: Memory = DurableMemory(LeakyStore(), "agent-a", SCOPE)
  assert await memory.search("private note") == []
