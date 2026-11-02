"""The host's side of a memory write, outside the runner."""
from pathlib import Path

import pytest

from deepstrike import MemoryCandidate, MemoryProvenance, MemoryRecord, MemoryScope
from deepstrike.memory.durable_lifecycle_store import FileMemoryLifecycleStore
from deepstrike.memory.lifecycle import MemoryLease
from deepstrike.memory.writer import MemoryWriter

SCOPE = MemoryScope("t", "n")


def record(record_id: str) -> MemoryRecord:
    return MemoryRecord(record_id, SCOPE, "brief-style", "project", "prefers numbered sections", "",
                        MemoryProvenance("model", "untrusted"), 1, 1)


class CountingStore:
    def __init__(self) -> None:
        self.puts: list[str] = []

    async def put(self, agent_id: str, memory: MemoryRecord) -> None:
        self.puts.append(memory.record_id)


@pytest.mark.asyncio
async def test_writes_once_per_write_id_within_the_lease_and_refuses_past_its_budget(tmp_path: Path):
    store = CountingStore()
    lifecycle = FileMemoryLifecycleStore(tmp_path / "leases.json")
    await lifecycle.issue_lease(MemoryLease("l", "agent", SCOPE, 0, 10_000, 1))
    writer = MemoryWriter(store=store, lifecycle=lifecycle, now=lambda: 5)
    receipt = await writer.persist("agent", record("memory:1"), "write-1")
    assert receipt["record_ref"] == "memory:1" and receipt["digest"].startswith("sha256:")
    # A redelivered write spends the budget once.
    await writer.persist("agent", record("memory:1"), "write-1")
    with pytest.raises(RuntimeError, match="memory lease rejected"):
        await writer.persist("agent", record("memory:2"), "write-2")
    assert store.puts == ["memory:1", "memory:1"]


@pytest.mark.asyncio
async def test_refuses_a_sink_answer_that_breaks_the_receipt_contract():
    class Answering:
        def __init__(self, answer):
            self.answer = answer

        async def submit(self, agent_id, candidate):
            return dict(self.answer)

    candidate = MemoryCandidate("c", "model_proposal", record("memory:1"))
    propose = lambda answer: MemoryWriter(sink=Answering(answer)).propose("agent", candidate)
    assert (await propose({"status": "pending", "candidate_id": "c", "receipt_id": "r"}))["status"] == "pending"
    for answer, message in [
        ({"status": "pending", "candidate_id": "other", "receipt_id": "r"}, "without a receipt for c"),
        ({"status": "accepted", "candidate_id": "c", "receipt_id": "r"}, "record_ref/digest"),
        ({"status": "rejected", "candidate_id": "c", "receipt_id": "r"}, "reason_code"),
        ({"status": "maybe", "candidate_id": "c", "receipt_id": "r"}, "unknown status"),
    ]:
        with pytest.raises(ValueError, match=message):
            await propose(answer)
    with pytest.raises(RuntimeError, match="memory_candidate_sink"):
        await MemoryWriter().propose("agent", candidate)
