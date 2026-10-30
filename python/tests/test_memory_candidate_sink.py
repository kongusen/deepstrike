from dataclasses import replace
import hashlib
from pathlib import Path
import json

import pytest

from deepstrike import (
    InMemoryMemoryCandidateSink, MemoryCandidate, MemoryCandidateSink,
    MemoryProvenance, MemoryRecord, MemoryScope,
    memory_candidate_id, memory_candidate_payload_digest,
)


def record(content="prefers numbered sections"):
    return MemoryRecord("r", MemoryScope("t", "n"), "brief-style", "project", content, "",
                        MemoryProvenance("model", "untrusted"), 1, 1)


def test_identity_is_compact_json_sha256_and_preserves_boundaries():
    assert memory_candidate_id(["a", "中"]) == "candidate:" + hashlib.sha256('["a","中"]'.encode()).hexdigest()
    assert memory_candidate_id(["a", "bc"]) != memory_candidate_id(["ab", "c"])
    assert memory_candidate_payload_digest(record()) == memory_candidate_payload_digest(replace(record(), record_id="other", created_at=99))


@pytest.mark.asyncio
async def test_pending_redelivery_conflict_and_order():
    sink = InMemoryMemoryCandidateSink()
    assert isinstance(sink, MemoryCandidateSink)
    candidate = MemoryCandidate("c1", "model_proposal", record())
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
    c = MemoryCandidate("same", "model_proposal", record())
    a, b = await sink.submit("a", c), await sink.submit("b", c)
    assert a["receipt_id"] != b["receipt_id"]
    assert sink.candidates("a") == [c] and sink.candidates("b") == [c]
    assert await sink.submit("a", c) == a


@pytest.mark.asyncio
async def test_source_and_review_metadata_changes_conflict():
    sink = InMemoryMemoryCandidateSink()
    c = MemoryCandidate("c", "model_proposal", record())
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


@pytest.mark.asyncio
@pytest.mark.asyncio
def test_ed25519_attestation_vector_is_canonical():
    from deepstrike.memory import MemoryCandidateAttestation, memory_candidate_attestation_canonical_payload, memory_candidate_attestation_digest
    fixture = json.loads((Path(__file__).parents[2] / "tests/fixtures/memory-attestation-ed25519.json").read_text())
    attestation = MemoryCandidateAttestation(
        "issuer-a", "ed-key-1", "agent", "t", "n", "host-candidate", "model_proposal",
        fixture["payload_digest"], 1000, 2000, "memory-attestation-v1", fixture["signature"])
    assert memory_candidate_attestation_canonical_payload(attestation) == fixture["canonical"]
    assert memory_candidate_attestation_digest(attestation) == fixture["attestation_digest"]


@pytest.mark.asyncio
async def test_host_source_attestation_is_required_and_verified(tmp_path: Path):
    from deepstrike.memory import (
        FileMemoryCandidateSink, HmacMemoryCandidateSourceVerifier, MemoryCandidateAttestation,
        sign_memory_candidate_attestation,
    )
    host_record = replace(record("host fact"), provenance=MemoryProvenance("host", "host_verified"))
    candidate = MemoryCandidate("host-candidate", "model_proposal", host_record)
    verifier = HmacMemoryCandidateSourceVerifier({"key-1": {"issuer": "issuer-a", "secret": "secret"}}, now=lambda: 1500)
    missing = await FileMemoryCandidateSink(tmp_path / "missing.json", source_verifier=verifier).submit("agent", candidate)
    assert missing["reason_code"] == "attestation_missing"
    unsigned = MemoryCandidateAttestation("issuer-a", "key-1", "agent", "t", "n", candidate.candidate_id, candidate.source,
                                          memory_candidate_payload_digest(host_record), 1000, 2000, "memory-attestation-v1", "")
    signed = replace(unsigned, signature=sign_memory_candidate_attestation(unsigned, "secret"))
    accepted = await FileMemoryCandidateSink(tmp_path / "valid.json", source_verifier=verifier).submit(
        "agent", replace(candidate, attestation=signed))
    assert accepted["status"] == "pending"


@pytest.mark.asyncio
async def test_file_sink_recovers_terminal_receipt_and_outbox(tmp_path: Path):
    from deepstrike.memory import FileMemoryCandidateSink

    path = tmp_path / "ledger.json"
    first = FileMemoryCandidateSink(path)
    pending = await first.submit("agent", MemoryCandidate("durable", "model_proposal", record()))
    accepted = await first.resolve("agent", "durable", {"status": "accepted", "record_ref": "memory:1", "digest": "sha256:record"})
    recovered = FileMemoryCandidateSink(path)
    assert await recovered.submit("agent", MemoryCandidate("durable", "model_proposal", record())) == accepted
    assert await recovered.resolve("agent", "durable", {"status": "rejected", "reason_code": "late"}) == accepted
    assert [event.type for event in await recovered.outbox()] == ["candidate_submitted", "candidate_accepted"]
    assert await recovered.ack_outbox("outbox:1")
    assert [event.event_id for event in await recovered.outbox()] == ["outbox:2"]


@pytest.mark.asyncio
async def test_file_sink_rejects_corrupt_snapshot(tmp_path: Path):
    from deepstrike.memory import FileMemoryCandidateSink

    path = tmp_path / "ledger.json"
    path.write_text("{broken", encoding="utf-8")
    with pytest.raises(ValueError):
        await FileMemoryCandidateSink(path).list()


MODEL_PROPOSAL_VECTOR = "candidate:d1d97d2e75c195fac9ca2bf3148de7ac88124f98d3517cf7038094eb0645a326"


def test_model_proposal_candidate_id_is_scoped_and_matches_the_other_sdks():
    proposed = replace(record(), confidence=0.5)
    candidate_id = memory_candidate_id(
        ["v3", "agent", "model_proposal", "s-1", "op-1", memory_candidate_payload_digest(proposed)])
    assert memory_candidate_id(["v3", "agent", "model_proposal", "s-1", "op-2", memory_candidate_payload_digest(proposed)]) != candidate_id
    assert memory_candidate_id(["v3", "agent", "model_proposal", "s-2", "op-1", memory_candidate_payload_digest(proposed)]) != candidate_id
    assert memory_candidate_id(["v3", "other", "model_proposal", "s-1", "op-1", memory_candidate_payload_digest(proposed)]) != candidate_id
    assert candidate_id == MODEL_PROPOSAL_VECTOR


@pytest.mark.asyncio
async def test_retried_model_proposal_with_a_different_answer_proposes_new_candidates():
    sink = InMemoryMemoryCandidateSink()

    def propose(content, **overrides):
        proposed = replace(record(content), **overrides)
        return MemoryCandidate(memory_candidate_id(["v3", "agent", "model_proposal", "s-1", "op-1", memory_candidate_payload_digest(proposed)]),
                               "model_proposal", proposed)

    first = await sink.submit("agent", propose("prefers numbered sections"))
    # The retry re-asks the provider model: the same proposal repeats the id and gets the
    # original receipt; a changed answer is a different candidate, not a conflict.
    same = await sink.submit("agent", propose("prefers numbered sections", record_id="retry", updated_at=9))
    changed = await sink.submit("agent", propose("prefers numbered headings"))
    assert same == first
    assert changed["status"] == "pending"
    assert changed["candidate_id"] != first["candidate_id"]
    assert len(sink.candidates("agent")) == 2
