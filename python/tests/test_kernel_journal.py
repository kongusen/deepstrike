"""Contract + atomicity tests for the `KernelJournal` capability (Canonical Kernel ABI §9.1).

Mirrors `node/tests/runtime/kernel-journal.test.ts` case for case: the contract suite runs against
both default implementations, and the file-backed suite proves that the *storage layer* — not a
userspace pre-check — decides every CAS.
"""

from __future__ import annotations

import asyncio
import json
import os
import threading
from pathlib import Path

import pytest

from deepstrike.runtime.kernel_journal import (
    CheckpointCandidate,
    FileKernelJournal,
    InMemoryKernelJournal,
    JournalCasConflictError,
    JournalHead,
    JournalIntegrityError,
    JournalRecordInput,
    KernelJournal,
)
from deepstrike.runtime.kernel_transaction_log import (
    create_kernel_operation_genesis,
    create_kernel_transaction,
)
from deepstrike.runtime.session_log import (
    FileSessionLog,
    InMemorySessionLog,
    journal_operation_key,
)

OP = "op-journal"


def record(step_seq: int, digest: str, payload: str | None = None) -> JournalRecordInput:
    """A record shaped like core's output: opaque bytes + the digest core assigned them."""
    return JournalRecordInput(
        step_seq=step_seq,
        record_digest=digest,
        record_bytes=(payload if payload is not None else f"payload-{step_seq}").encode("utf-8"),
    )


def candidate(checkpoint_id: str, through_step_seq: int) -> CheckpointCandidate:
    return CheckpointCandidate(
        checkpoint_id=checkpoint_id,
        through_step_seq=through_step_seq,
        state_digest=f"state-{checkpoint_id}",
        checkpoint_bytes=f"checkpoint-{checkpoint_id}".encode("utf-8"),
    )


async def seed_chain(journal: KernelJournal, count: int, operation_id: str = OP) -> list[str]:
    """Append `count` linked records after genesis; returns every digest in chain order."""
    digests = ["d0"]
    await journal.compare_and_append(operation_id, None, record(0, "d0"))
    for seq in range(1, count + 1):
        await journal.compare_and_append(operation_id, digests[seq - 1], record(seq, f"d{seq}"))
        digests.append(f"d{seq}")
    return digests


# ------------------------------------------------------------------ #
# KernelJournal contract — both default implementations
# ------------------------------------------------------------------ #


@pytest.fixture(params=["InMemoryKernelJournal", "FileKernelJournal"])
def journal(request, tmp_path: Path) -> KernelJournal:
    if request.param == "InMemoryKernelJournal":
        return InMemoryKernelJournal()
    return FileKernelJournal(tmp_path / "journal")


async def test_genesis_append_starts_the_chain_and_advances_the_head(journal: KernelJournal):
    assert await journal.head(OP) is None

    receipt = await journal.compare_and_append(OP, None, record(0, "d0"))
    assert (receipt.step_seq, receipt.record_digest) == (0, "d0")
    assert await journal.head(OP) == JournalHead(step_seq=0, record_digest="d0")

    await journal.compare_and_append(OP, "d0", record(1, "d1"))
    assert await journal.head(OP) == JournalHead(step_seq=1, record_digest="d1")


async def test_stores_record_bytes_verbatim_and_links_each_cas_precondition(journal: KernelJournal):
    await seed_chain(journal, 2)
    entries = await journal.read_from(OP)

    assert [entry.step_seq for entry in entries] == [0, 1, 2]
    assert [entry.previous_record_digest for entry in entries] == [None, "d0", "d1"]
    assert entries[2].record_bytes == b"payload-2"


async def test_rejects_a_stale_expected_head_without_overwriting(journal: KernelJournal):
    await seed_chain(journal, 1)

    with pytest.raises(JournalCasConflictError):
        await journal.compare_and_append(OP, "d0", record(1, "other"))

    # The winner is untouched: same head, same bytes, no fork.
    assert await journal.head(OP) == JournalHead(step_seq=1, record_digest="d1")
    assert len(await journal.read_from(OP)) == 2


async def test_rejects_a_second_genesis_on_a_non_empty_chain(journal: KernelJournal):
    await journal.compare_and_append(OP, None, record(0, "d0"))
    with pytest.raises(JournalCasConflictError):
        await journal.compare_and_append(OP, None, record(0, "other"))


async def test_separates_a_cas_conflict_from_an_integrity_violation(journal: KernelJournal):
    await seed_chain(journal, 1)

    # Head matches, but the record claims a position that does not follow it.
    with pytest.raises(JournalIntegrityError):
        await journal.compare_and_append(OP, "d1", record(5, "d5"))
    # A genesis record on a chain that already has one is a conflict, not an integrity fault.
    with pytest.raises(JournalCasConflictError):
        await journal.compare_and_append(OP, None, record(0, "d0"))


async def test_reads_by_step_cursor_and_by_digest_cursor(journal: KernelJournal):
    digests = await seed_chain(journal, 3)

    assert [entry.step_seq for entry in await journal.read_from(OP, 2)] == [2, 3]
    assert [entry.step_seq for entry in await journal.records_after(OP, digests[1])] == [2, 3]
    assert [entry.step_seq for entry in await journal.records_after(OP)] == [0, 1, 2, 3]
    with pytest.raises(JournalIntegrityError):
        await journal.records_after(OP, "not-a-record")


async def test_keeps_operations_isolated(journal: KernelJournal):
    await seed_chain(journal, 1, "op-a")
    await seed_chain(journal, 2, "op-b")

    assert await journal.head("op-a") == JournalHead(step_seq=1, record_digest="d1")
    assert await journal.head("op-b") == JournalHead(step_seq=2, record_digest="d2")


async def test_installs_a_checkpoint_whose_covered_head_is_no_longer_current(journal: KernelJournal):
    """§22.14: the covered head is verified against `through_step_seq`, not against the live head."""
    digests = await seed_chain(journal, 3)

    # Candidate covers step 1; steps 2 and 3 were appended after it was taken.
    installed = await journal.compare_and_install_checkpoint(
        OP, None, digests[1], candidate("ck-1", 1)
    )

    assert installed.ordinal == 0
    assert installed.covered_head == "d1"
    assert installed.acknowledged is False
    assert await journal.head(OP) == JournalHead(step_seq=3, record_digest="d3")
    latest = await journal.latest_checkpoint(OP)
    assert latest is not None and latest.checkpoint_id == "ck-1"
    # The tail survives the install.
    assert [entry.step_seq for entry in await journal.read_from(OP)] == [0, 1, 2, 3]


async def test_rejects_a_checkpoint_whose_covered_head_disagrees(journal: KernelJournal):
    digests = await seed_chain(journal, 2)

    with pytest.raises(JournalIntegrityError):
        await journal.compare_and_install_checkpoint(OP, None, digests[2], candidate("ck-1", 1))
    with pytest.raises(JournalIntegrityError):
        await journal.compare_and_install_checkpoint(OP, None, "d9", candidate("ck-1", 9))
    assert await journal.latest_checkpoint(OP) is None


async def test_advances_the_checkpoint_pointer_monotonically_under_cas(journal: KernelJournal):
    digests = await seed_chain(journal, 3)
    await journal.compare_and_install_checkpoint(OP, None, digests[1], candidate("ck-1", 1))

    # Installing again without naming the predecessor is a conflict.
    with pytest.raises(JournalCasConflictError):
        await journal.compare_and_install_checkpoint(OP, None, digests[2], candidate("ck-2", 2))
    # Naming a stale predecessor is a conflict.
    with pytest.raises(JournalCasConflictError):
        await journal.compare_and_install_checkpoint(OP, "ck-0", digests[2], candidate("ck-2", 2))
    # Naming the current predecessor but moving backwards is an integrity fault.
    with pytest.raises(JournalIntegrityError):
        await journal.compare_and_install_checkpoint(OP, "ck-1", digests[0], candidate("ck-2", 0))

    second = await journal.compare_and_install_checkpoint(
        OP, "ck-1", digests[2], candidate("ck-2", 2)
    )
    assert second.ordinal == 1
    assert second.previous_checkpoint_id == "ck-1"
    latest = await journal.latest_checkpoint(OP)
    assert latest is not None and latest.checkpoint_id == "ck-2"


async def test_gates_prefix_reclamation_on_the_acknowledgement_boundary(journal: KernelJournal):
    digests = await seed_chain(journal, 3)
    await journal.compare_and_install_checkpoint(OP, None, digests[2], candidate("ck-1", 2))

    # Installed but unacknowledged: nothing is reclaimed.
    receipt = await journal.prune_acked_prefix(OP)
    assert (receipt.pruned_through_step_seq, receipt.pruned_count) == (-1, 0)
    assert len(await journal.read_from(OP)) == 4

    acked = await journal.ack_checkpoint(OP, "ck-1")
    assert acked.acknowledged is True
    latest = await journal.latest_checkpoint(OP)
    assert latest is not None and latest.acknowledged is True

    receipt = await journal.prune_acked_prefix(OP)
    assert (receipt.pruned_through_step_seq, receipt.pruned_count) == (2, 3)
    assert [entry.step_seq for entry in await journal.read_from(OP)] == [3]
    # The pruned boundary is retained as an anchor, so head and the digest cursor still resolve.
    assert await journal.head(OP) == JournalHead(step_seq=3, record_digest="d3")
    assert [entry.step_seq for entry in await journal.records_after(OP, digests[2])] == [3]
    # And the chain keeps growing from the surviving head.
    await journal.compare_and_append(OP, "d3", record(4, "d4"))
    assert await journal.head(OP) == JournalHead(step_seq=4, record_digest="d4")


async def test_refuses_to_acknowledge_an_uninstalled_checkpoint(journal: KernelJournal):
    await seed_chain(journal, 1)
    with pytest.raises(JournalIntegrityError):
        await journal.ack_checkpoint(OP, "ck-missing")


# ------------------------------------------------------------------ #
# FileKernelJournal — cross-process atomicity
#
# The race is run on real OS threads, each with its own event loop and its own journal instance, and
# each held at a shared barrier until every writer has observed the same head. `link(2)` is atomic
# against any other opener of the same directory — thread or process alike — so a thread that loses
# here would also lose across a process boundary; what the barrier removes is the possibility that
# the in-process pre-check, rather than the storage layer, resolved the race.
# ------------------------------------------------------------------ #


class _BarrieredAppend(FileKernelJournal):
    """Holds every writer at the same observed head, so only the atomic claim can break the tie."""

    def __init__(self, root, barrier: threading.Barrier) -> None:
        super().__init__(root)
        self._barrier = barrier

    async def head(self, operation_id: str):
        observed = await super().head(operation_id)
        self._barrier.wait()
        return observed


class _BarrieredInstall(FileKernelJournal):
    """Same, for the checkpoint ordinal space."""

    def __init__(self, root, barrier: threading.Barrier) -> None:
        super().__init__(root)
        self._barrier = barrier

    async def latest_checkpoint(self, operation_id: str):
        observed = await super().latest_checkpoint(operation_id)
        self._barrier.wait()
        return observed


def run_concurrently(factories) -> list[tuple[str, object]]:
    """Run each coroutine factory on its own thread + event loop; collect ("ok"|"err", value)."""
    results: list[tuple[str, object] | None] = [None] * len(factories)

    def worker(index: int, factory) -> None:
        try:
            results[index] = ("ok", asyncio.run(factory()))
        except BaseException as err:  # noqa: BLE001 - the failure is the assertion subject
            results[index] = ("err", err)

    threads = [threading.Thread(target=worker, args=(i, f)) for i, f in enumerate(factories)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(timeout=60)
    assert all(result is not None for result in results), "a concurrent writer never finished"
    return results  # type: ignore[return-value]


def test_exactly_one_of_two_concurrent_writers_appends_at_the_same_position(tmp_path: Path):
    root = tmp_path / "journal"
    asyncio.run(FileKernelJournal(root).compare_and_append(OP, None, record(0, "d0")))

    barrier = threading.Barrier(2, timeout=60)
    writers = [_BarrieredAppend(root, barrier), _BarrieredAppend(root, barrier)]
    results = run_concurrently(
        [
            (lambda w=w, i=i: w.compare_and_append(OP, "d0", record(1, f"from-{i}", f"body-{i}")))
            for i, w in enumerate(writers)
        ]
    )

    winners = [value for status, value in results if status == "ok"]
    losers = [value for status, value in results if status == "err"]
    assert len(winners) == 1
    assert isinstance(losers[0], JournalCasConflictError)
    # Both writers passed their preconditions against the same head — the loser lost at `link(2)`,
    # which is the whole point: acceptance is decided by the storage layer, not in userspace.
    assert "claimed by a concurrent writer" in str(losers[0])

    # The journal did not fork: one record at step 1, and every reader agrees on it.
    entries = asyncio.run(FileKernelJournal(root).read_from(OP))
    assert [entry.step_seq for entry in entries] == [0, 1]
    assert entries[1].record_digest == winners[0].record_digest
    for writer in writers:
        assert asyncio.run(FileKernelJournal(root).head(OP)).record_digest == winners[0].record_digest


def test_survives_a_wide_concurrent_append_storm_with_a_single_winner(tmp_path: Path):
    root = tmp_path / "journal"
    asyncio.run(FileKernelJournal(root).compare_and_append(OP, None, record(0, "d0")))

    barrier = threading.Barrier(8, timeout=60)
    writers = [_BarrieredAppend(root, barrier) for _ in range(8)]
    results = run_concurrently(
        [
            (lambda w=w, i=i: w.compare_and_append(OP, "d0", record(1, f"d1-{i}", f"w{i}")))
            for i, w in enumerate(writers)
        ]
    )

    assert len([1 for status, _ in results if status == "ok"]) == 1
    for status, value in results:
        if status == "err":
            assert isinstance(value, JournalCasConflictError)
            assert "claimed by a concurrent writer" in str(value)
    assert len(asyncio.run(FileKernelJournal(root).read_from(OP))) == 2


def test_exactly_one_of_two_concurrent_writers_installs_a_checkpoint(tmp_path: Path):
    root = tmp_path / "journal"
    digests = asyncio.run(seed_chain(FileKernelJournal(root), 2))

    barrier = threading.Barrier(2, timeout=60)
    installers = [_BarrieredInstall(root, barrier), _BarrieredInstall(root, barrier)]
    results = run_concurrently(
        [
            (
                lambda w=w, name=name: w.compare_and_install_checkpoint(
                    OP, None, digests[2], candidate(name, 2)
                )
            )
            for w, name in zip(installers, ["ck-a", "ck-b"])
        ]
    )

    assert len([1 for status, _ in results if status == "ok"]) == 1
    losers = [value for status, value in results if status == "err"]
    assert isinstance(losers[0], JournalCasConflictError)
    assert "claimed by a concurrent installer" in str(losers[0])

    installed = asyncio.run(FileKernelJournal(root).latest_checkpoint(OP))
    assert installed is not None
    assert installed.checkpoint_id in {"ck-a", "ck-b"}
    assert installed.ordinal == 0
    assert os.listdir(root / OP / "checkpoints") == ["000000000000.ckpt"]


async def test_the_atomic_claim_not_the_pre_check_decides_the_append(tmp_path: Path):
    """The pre-`link` head read cannot be the fence: another process may commit between it and the
    publish. Simulate exactly that interleaving by holding the pre-check's view at a stale head while
    the position it computes is already taken. If acceptance were decided in userspace this append
    would succeed and fork the chain; it must lose to the storage layer instead.
    """
    root = tmp_path / "journal"

    class StalePreCheck(FileKernelJournal):
        async def head(self, operation_id: str):
            return JournalHead(step_seq=0, record_digest="d0")

    committed = FileKernelJournal(root)
    await committed.compare_and_append(OP, None, record(0, "d0"))
    await committed.compare_and_append(OP, "d0", record(1, "winner", "winner"))

    with pytest.raises(JournalCasConflictError):
        await StalePreCheck(root).compare_and_append(OP, "d0", record(1, "loser", "loser"))

    entries = await committed.read_from(OP)
    assert [entry.record_digest for entry in entries] == ["d0", "winner"]
    assert entries[1].record_bytes == b"winner"


async def test_the_atomic_claim_decides_the_checkpoint_install_too(tmp_path: Path):
    root = tmp_path / "journal"

    class StalePreCheck(FileKernelJournal):
        async def latest_checkpoint(self, operation_id: str):
            return None

    committed = FileKernelJournal(root)
    digests = await seed_chain(committed, 2)
    await committed.compare_and_install_checkpoint(OP, None, digests[2], candidate("ck-winner", 2))

    with pytest.raises(JournalCasConflictError):
        await StalePreCheck(root).compare_and_install_checkpoint(
            OP, None, digests[2], candidate("ck-loser", 2)
        )

    latest = await committed.latest_checkpoint(OP)
    assert latest is not None and latest.checkpoint_id == "ck-winner"


async def test_reopens_and_verifies_the_chain_ignoring_crash_residue(tmp_path: Path):
    root = tmp_path / "journal"
    journal = FileKernelJournal(root)
    digests = await seed_chain(journal, 3)

    # Residue a crash can actually leave: a staged-but-unlinked temp file, plus anything that does
    # not match the record naming rule. Neither may be mistaken for a committed record.
    (root / OP / "tmp").mkdir(parents=True, exist_ok=True)
    (root / OP / "tmp" / "half-written.tmp").write_text('{"step_seq":4,"record_dig', encoding="utf-8")
    (root / OP / "records" / "000000000004.rec.partial").write_text('{"step_seq":4', encoding="utf-8")
    (root / OP / "records" / "notes.txt").write_text("scratch", encoding="utf-8")

    reopened = FileKernelJournal(root)
    entries = await reopened.read_from(OP)
    assert [entry.step_seq for entry in entries] == [0, 1, 2, 3]
    assert [entry.record_digest for entry in entries] == digests
    assert await reopened.head(OP) == JournalHead(step_seq=3, record_digest="d3")
    # The chain still accepts its next record, so residue did not poison the CAS position either.
    await reopened.compare_and_append(OP, "d3", record(4, "d4"))
    assert await reopened.head(OP) == JournalHead(step_seq=4, record_digest="d4")


async def test_reopens_installed_and_acknowledged_checkpoints(tmp_path: Path):
    root = tmp_path / "journal"
    journal = FileKernelJournal(root)
    digests = await seed_chain(journal, 2)
    await journal.compare_and_install_checkpoint(OP, None, digests[1], candidate("ck-1", 1))
    await journal.ack_checkpoint(OP, "ck-1")

    latest = await FileKernelJournal(root).latest_checkpoint(OP)
    assert latest is not None
    assert latest.checkpoint_id == "ck-1"
    assert latest.acknowledged is True
    assert latest.covered_head == "d1"
    assert latest.through_step_seq == 1
    assert latest.checkpoint_bytes == b"checkpoint-ck-1"


async def test_raises_an_integrity_fault_when_a_record_contradicts_its_own_name(tmp_path: Path):
    root = tmp_path / "journal"
    await seed_chain(FileKernelJournal(root), 1)
    (root / OP / "records" / "000000000002.rec").write_text(
        json.dumps({"step_seq": 7}), encoding="utf-8"
    )

    with pytest.raises(JournalIntegrityError):
        await FileKernelJournal(root).read_from(OP)


# ------------------------------------------------------------------ #
# SessionLog / KernelJournal capability separation
# ------------------------------------------------------------------ #


def operation_genesis(operation_id: str = "op-1"):
    return create_kernel_operation_genesis(
        abi_version=2,
        operation_id=operation_id,
        initial_scheduler_policy={"max_tokens": 8_000},
        resolved_runtime_defaults={"max_input_bytes": 16_777_216},
        default_policy_version=1,
    )


def transaction(previous_transaction_digest: str, step_seq: int = 1):
    return create_kernel_transaction(
        operation_id="op-1",
        step_seq=step_seq,
        base_generation=step_seq - 1,
        input={"version": 2, "operation_id": "op-1", "event_id": f"event-{step_seq}"},
        step={"version": 2, "operation_id": "op-1", "step_seq": step_seq, "actions": []},
        previous_transaction_digest=previous_transaction_digest,
    )


async def test_journal_is_exposed_as_its_own_capability_not_as_session_log_methods():
    log = InMemorySessionLog()
    genesis = operation_genesis()
    await log.append_kernel_genesis("s1", genesis)

    # The same durable chain is reachable through the separated capability.
    head = await log.kernel_journal.head(journal_operation_key("s1", "op-1"))
    assert head == JournalHead(step_seq=0, record_digest=genesis["genesis_digest"])


async def test_journal_records_and_business_events_use_independent_sequence_spaces():
    log = InMemorySessionLog()
    genesis = operation_genesis()

    # Interleave: genesis, event, transaction, event, transaction.
    genesis_receipt = await log.append_kernel_genesis("s1", genesis)
    event0 = await log.append("s1", {"kind": "run_started", "run_id": "r1", "goal": "a", "criteria": []})
    first = transaction(genesis["genesis_digest"])
    first_receipt = await log.compare_and_append_kernel_transaction(
        "s1", genesis["genesis_digest"], first
    )
    event1 = await log.append("s1", {"kind": "llm_completed", "turn": 0, "content": "b", "tool_calls": []})
    second = transaction(first["transaction_digest"], 2)
    second_receipt = await log.compare_and_append_kernel_transaction(
        "s1", first["transaction_digest"], second
    )

    # Business events: a dense 0,1 — journal appends never consumed a business number.
    assert [event0, event1] == [0, 1]
    assert await log.latest_seq("s1") == 1
    assert [entry.seq for entry in await log.read("s1")] == [0, 1]

    # Journal records: their own dense chain positions, unaffected by the interleaved events.
    assert genesis_receipt["log_seq"] == 0
    assert first_receipt["log_seq"] == 1
    assert second_receipt["log_seq"] == 2


async def test_file_backed_pair_keeps_the_same_sequence_space_split(tmp_path: Path):
    log = FileSessionLog(tmp_path)
    genesis = operation_genesis()
    genesis_receipt = await log.append_kernel_genesis("sess", genesis)
    event0 = await log.append("sess", {"kind": "run_started", "run_id": "r1", "goal": "a", "criteria": []})
    first = transaction(genesis["genesis_digest"])
    receipt = await log.compare_and_append_kernel_transaction("sess", genesis["genesis_digest"], first)
    event1 = await log.append("sess", {"kind": "llm_completed", "turn": 0, "content": "b", "tool_calls": []})

    assert [event0, event1] == [0, 1]
    assert genesis_receipt["log_seq"] == 0
    assert receipt["log_seq"] == 1

    # The projection file holds only business events; the journal lives beside it.
    reopened = FileSessionLog(tmp_path)
    assert [entry.seq for entry in await reopened.read("sess")] == [0, 1]
    assert await reopened.latest_seq("sess") == 1
    assert await reopened.kernel_transaction_head("sess", "op-1") == first["transaction_digest"]
    assert {"kernel-journal", "sess.jsonl"} <= set(os.listdir(tmp_path))
