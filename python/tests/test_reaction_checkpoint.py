import pytest

from deepstrike import InMemoryReactionCheckpointStore, ReactionRecord


@pytest.mark.asyncio
async def test_only_one_worker_claims_and_stale_writes_fail_after_expiry():
    now = 1_000
    store = InMemoryReactionCheckpointStore(now=lambda: now, default_lease_ms=100)
    first = await store.claim("event")
    assert first.status == "claimed"
    assert (await store.claim("event")).status == "busy"
    await store.save_plan(first.claim, ["alice"])

    now += 101
    second = await store.claim("event")
    assert second.status == "claimed"
    assert await store.record(first.claim, ReactionRecord("alice", "stale")) is False
    assert await store.record(second.claim, ReactionRecord("alice", "fresh")) is True
    assert await store.complete(second.claim) is True

    completed = await store.claim("event")
    assert completed.status == "completed"
    assert completed.reactions == [ReactionRecord("alice", "fresh")]
