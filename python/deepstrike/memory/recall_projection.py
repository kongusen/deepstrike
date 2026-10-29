"""M5 · how a recalled record enters the model's context.

A host prefetch (turn 1 and after each sprint renewal) lands every accepted hit in ``history`` as
its own ordinary user turn — single-use retrieval content that decays with the compression
pyramid, never pinned into ``knowledge`` (docs/guides/context-engineering.md, Level 5). One rule in
every SDK, pinned by the same vector in each SDK's test.
"""

from __future__ import annotations

from deepstrike.memory.protocols import MemoryRecord


def memory_recall_turn(record: MemoryRecord) -> str:
    """The history turn one recalled record becomes.

    The header names the record and its trust level so the model can tell host-verified facts from
    untrusted ones; the retrieval score is left out because it changes between retrievals while
    the record does not.
    """
    return (f"[MEMORY record_id={record.record_id} kind={record.kind} "
            f"trust={record.provenance.trust}] {record.content}")
