from dataclasses import replace
import hashlib

import pytest

from deepstrike import (
    InMemoryMemoryCandidateSink, MemoryCandidate, MemoryCandidateSink,
    MemoryProvenance, MemoryRecord, MemoryScope,
    memory_candidate_id, memory_candidate_payload_digest,
)


def record(content="prefers numbered sections"):
    return MemoryRecord("r", MemoryScope("t", "n"), "brief-style", "project", content, "",
                        MemoryProvenance("extraction", "untrusted"), 1, 1)


def test_identity_is_compact_json_sha256_and_preserves_boundaries():
    assert memory_candidate_id(["a", "中"]) == "candidate:" + hashlib.sha256('["a","中"]'.encode()).hexdigest()
    assert memory_candidate_id(["a", "bc"]) != memory_candidate_id(["ab", "c"])
    assert memory_candidate_payload_digest(record()) == memory_candidate_payload_digest(replace(record(), record_id="other", created_at=99))


@pytest.mark.asyncio
async def test_pending_redelivery_conflict_and_order():
    sink = InMemoryMemoryCandidateSink()
    assert isinstance(sink, MemoryCandidateSink)
    candidate = MemoryCandidate("c1", "session_extraction", record())
    first = await sink.submit("agent", candidate)
    assert first["status"] == "pending" and first["receipt_id"]
    again = await sink.submit("agent", replace(candidate, record=replace(record(), record_id="other", updated_at=99)))
    assert again == first
    conflict = await sink.submit("agent", replace(candidate, record=record("prefers prose")))
    assert conflict["status"] == "rejected"
    assert conflict["reason_code"] == "candidate_conflict"
    assert conflict["receipt_id"] != first["receipt_id"]
    await sink.submit("other", replace(candidate, candidate_id="c2"))
    assert [c.candidate_id for c in sink.candidates()] == ["c1", "c2"]
    assert sink.candidates("agent") == [candidate]
    assert await sink.submit("agent", candidate) == first


@pytest.mark.asyncio
async def test_caller_mutation_cannot_change_held_proposal_or_receipt():
    sink = InMemoryMemoryCandidateSink()
    candidate = MemoryCandidate("c1", "child_request", record())
    receipt = await sink.submit("a", candidate)
    receipt["receipt_id"] = "tampered"
    candidate.record.content = "tampered"
    sink.candidates()[0].record.content = "also tampered"
    retry = await sink.submit("a", MemoryCandidate("c1", "child_request", record()))
    assert retry["receipt_id"] == "receipt:1"
    assert sink.candidates()[0].record.content == "prefers numbered sections"

@pytest.mark.asyncio
async def test_identical_id_is_scoped_by_agent():
    sink = InMemoryMemoryCandidateSink()
    c = MemoryCandidate("same", "session_extraction", record())
    a, b = await sink.submit("a", c), await sink.submit("b", c)
    assert a["receipt_id"] != b["receipt_id"]
    assert sink.candidates("a") == [c] and sink.candidates("b") == [c]
    assert await sink.submit("a", c) == a


@pytest.mark.asyncio
async def test_source_and_review_metadata_changes_conflict():
    sink = InMemoryMemoryCandidateSink()
    c = MemoryCandidate("c", "session_extraction", record())
    original = await sink.submit("agent", c)
    alternatives = [replace(c, source="child_request")]
    for field, value in [("trust", "host_verified"), ("evidence_refs", ["new-evidence"]), ("session_id", "other")]:
        alternatives.append(replace(c, record=replace(c.record, provenance=replace(c.record.provenance, **{field: value}))))
    for field, value in [("confidence", .9), ("pinned", True), ("links", ["related"]), ("ttl_days", 7)]:
        alternatives.append(replace(c, record=replace(c.record, **{field: value})))
    for other in alternatives:
        assert (await sink.submit("agent", other))["reason_code"] == "candidate_conflict"
    assert await sink.submit("agent", c) == original
    assert sink.candidates() == [c]
