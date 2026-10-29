"""Shared evidence read model: Python mirror of node/tests/shared-evidence.test.ts.

Cross-SDK byte equality is pinned by the ``shared_evidence_snapshot`` conformance fixture; these
tests cover the local behaviour the fixture does not isolate.
"""
from __future__ import annotations

import hashlib
from typing import Any

import pytest

from deepstrike.runtime.session_log import InMemorySessionLog
from deepstrike.runtime.shared_evidence import (
  EvidenceLink,
  EvidenceRef,
  EvidenceScope,
  SessionEvidenceReader,
  canonical_event_json,
  create_shared_evidence,
  knowledge_identity,
  read_session_evidence,
)
from deepstrike.runtime.verifiable_report import VerifiableEvidence

SCOPE = {"agent_id": "agent-1", "tenant_id": "t1", "namespace": "ns"}
MEMORY_SCOPE = {"tenant_id": "t1", "namespace": "ns"}


def digest(name: str) -> str:
  return "sha256:" + name.ljust(64, "0")


def prepared(turn: int, entries: list[tuple[str, str]]) -> dict[str, Any]:
  return {
    "kind": "context_prepared",
    "turn": turn,
    "effect_id": f"effect-{turn}",
    "preparation": {
      "binding": {"digest": f"sha256:binding-{turn}"},
      "state": {"knowledge": [
        {"entry_id": f"knowledge:{ordinal}:{key}:{content}", "content_digest": content,
         "source": "knowledge", "ordinal": ordinal}
        for ordinal, (key, content) in enumerate(entries)
      ]},
    },
  }


async def log_of(*events: dict[str, Any]) -> InMemorySessionLog:
  log = InMemorySessionLog()
  for event in events:
    await log.append("s1", event)
  return log


def test_canonical_event_json_sorts_keys_and_drops_absent_members() -> None:
  assert canonical_event_json({"b": 1.0, "a": None, "c": [None, {"z": 2, "y": "é"}]}) == \
    '{"b":1,"c":[null,{"y":"é","z":2}]}'


def test_knowledge_identity_drops_the_shifting_ordinal() -> None:
  identity = knowledge_identity(f"knowledge:3:skill:debugging:{digest('a')}", digest("a"))
  assert (identity.ref_id, identity.key, identity.origin, identity.links) == \
    (f"skill:debugging@{digest('a')}", "skill:debugging", "skill", None)
  assert knowledge_identity("kb-1", "sha256:kb").ref_id == "kb-1@sha256:kb"
  memory = knowledge_identity(f"knowledge:0:memory:t1:ns:project:x:{digest('m')}", digest("m"))
  assert memory.links == (EvidenceLink("recalled_from", "memory_record", "t1:ns:project:x"),)


@pytest.mark.asyncio
async def test_recalled_knowledge_links_to_its_memory_record() -> None:
  record_id = "t1:ns:project:note"
  record = {"record_id": record_id, "scope": MEMORY_SCOPE, "pinned": False,
            "provenance": {"author": "extraction", "trust": "untrusted", "evidence_refs": []}}
  log = await log_of(
    {"kind": "memory_retrieval_result", "hits": [{"score": 1, "why": "w", "record": record}]},
    prepared(1, [("skill:debugging", digest("s")), (f"memory:{record_id}", digest("m"))]),
    prepared(2, [(f"memory:{record_id}", digest("m"))]),
  )
  reader = await SessionEvidenceReader.read(operation_id="op-1", session_id="s1", scope=SCOPE, session_log=log)
  knowledge = reader.refs("knowledge_entry")
  assert [ref.ref_id for ref in knowledge] == [f"memory:{record_id}@{digest('m')}", f"skill:debugging@{digest('s')}"]
  memory = reader.by_ref_id("memory_record", record_id)
  assert reader.linked(knowledge[0], "recalled_from") == (memory,)
  assert reader.linking(memory) == (knowledge[0],)
  assert memory.evidence_refs is None
  codes = [item.code for item in reader.snapshot.diagnostics]
  assert "knowledge_provenance_missing" not in codes
  assert "evidence_link_unresolved" not in codes


@pytest.mark.asyncio
async def test_unresolved_recall_link_makes_the_snapshot_partial() -> None:
  log = await log_of(prepared(1, [("memory:gone", digest("g"))]))
  snapshot = await read_session_evidence(operation_id="op-1", session_id="s1", scope=SCOPE, session_log=log)
  assert snapshot.completeness == "partial"
  assert ("evidence_link_unresolved", f"memory:gone@{digest('g')}") in [
    (item.code, item.ref_id) for item in snapshot.diagnostics]


@pytest.mark.asyncio
async def test_candidate_receipts_refine_link_and_conflict() -> None:
  log = await log_of(
    {"kind": "memory_candidate_pending", "turn": 1, "candidate_id": "a", "source": "session_extraction",
     "name": "n", "receipt_id": "r1"},
    {"kind": "memory_candidate_pending", "turn": 1, "candidate_id": "b", "source": "session_extraction",
     "name": "n", "receipt_id": "r2"},
    {"kind": "memory_written", "turn": 2, "record_id": "rec-1", "scope": MEMORY_SCOPE,
     "memory_kind": "project", "name": "n", "size_bytes": 3},
  )
  receipts = {
    "a": {"status": "accepted", "candidate_id": "a", "receipt_id": "r9", "record_ref": "rec-1", "digest": "sha256:r"},
    "b": {"status": "pending", "candidate_id": "b", "receipt_id": "r3"},
  }
  submitted: list[str] = []

  class Sidecar:
    async def memory_candidate_receipt(self, agent_id: str, candidate_id: str) -> dict[str, Any] | None:
      return receipts.get(candidate_id)

    def submit(self, *_: Any) -> None:
      submitted.append("submit")

  reader = await SessionEvidenceReader.read(
    operation_id="op-1", session_id="s1", scope=SCOPE, session_log=log, sidecar=Sidecar())
  accepted = reader.by_ref_id("memory_candidate", "a")
  assert (accepted.settlement, accepted.version, accepted.payload_ref) == ("accepted", "r9", "rec-1")
  assert accepted.links == (EvidenceLink("settled_as", "memory_record", "rec-1"),)
  renamed = reader.by_ref_id("memory_candidate", "b")
  assert (renamed.settlement, renamed.version) == ("pending", "r2")
  assert ("memory_candidate_receipt_conflict", "b") in [(d.code, d.ref_id) for d in reader.snapshot.diagnostics]
  assert submitted == []


@pytest.mark.asyncio
async def test_session_event_digest_covers_the_canonical_bytes() -> None:
  event = {"kind": "run_terminal", "reason": "completed", "turns_used": 1, "total_tokens": 2}
  snapshot = await read_session_evidence(
    operation_id="op-1", session_id="s1", scope=SCOPE, session_log=await log_of(event))
  data = snapshot.verification.session_logs[0][0]
  assert data == canonical_event_json(event).encode()
  ref = next(ref for ref in snapshot.refs if ref.domain == "session_event")
  assert ref.content_digest == "sha256:" + hashlib.sha256(data).hexdigest()
  assert [item.code for item in snapshot.diagnostics] == ["journal_not_provided"]


def test_builder_rejects_malformed_links_and_cross_scope_refs() -> None:
  scope = EvidenceScope(operation_id="op-1")
  bad_link = EvidenceRef(ref_id="r", domain="memory_record", authority="host_observation", scope=scope,
                         availability="external", links=(EvidenceLink("copied_from", "memory_record", "m"),))
  with pytest.raises(TypeError, match="evidence link.relation is invalid"):
    create_shared_evidence(operation_id="op-1", verification=VerifiableEvidence(journal=()), refs=[bad_link])
  foreign = EvidenceRef(ref_id="r", domain="memory_record", authority="host_observation",
                        scope=EvidenceScope(operation_id="op-2"), availability="external")
  with pytest.raises(ValueError, match="crosses the shared evidence scope"):
    create_shared_evidence(operation_id="op-1", verification=VerifiableEvidence(journal=()), refs=[foreign])
