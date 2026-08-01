"""`KernelJournal` — the durable transaction capability of the Canonical Kernel ABI (spec §9.1).

Python port of the authoritative Node implementation (`node/src/runtime/kernel-journal.ts`);
the interface shape is identical field-for-field, in snake_case.

Three rules give this module its shape:

1. **Records are opaque.** core owns canonical serialization and hashing
   (`KernelRecord::record_bytes()` / `record_digest()` / `expected_head()`). The host stores the
   bytes verbatim and indexes them by the digest core handed it. A journal that re-serializes a
   record to recompute its hash would make "the host recomputed and disagreed" a reachable state;
   it is not one here.
2. **CAS is a storage-layer primitive, not a read-compare-write sequence.** §9.1 requires a real
   atomic operation (file lock / `O_EXCL` chained naming / conditional database update).
   `InMemoryKernelJournal` is atomic only within one process and says so in its own docstring;
   `FileKernelJournal` is atomic across processes (see its class docs).
3. **The journal's sequence space is `step_seq`** — the operation's record-chain position — and is
   completely independent of `SessionLog`'s business event `seq`. Pruning a journal prefix can
   never punch a hole in business event numbering (spec Task 8b, criterion 4).

Failures are typed so a caller can tell "retry after rebuild" from "this storage is broken" from
"someone handed me a corrupt chain" — a durable-step wrapper must never publish effects on any of
them, and must never collapse them into one opaque exception.
"""

from __future__ import annotations

import base64
import json
import os
import re
import struct
import uuid
from abc import ABC, abstractmethod
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any

MAX_SAFE_INTEGER = 9_007_199_254_740_991


# ------------------------------------------------------------------ #
# Errors
# ------------------------------------------------------------------ #


class JournalCasConflictError(RuntimeError):
    """The CAS precondition did not hold: the journal head (or checkpoint pointer) moved.

    Retryable — the protocol response is `abort(token)` → re-read head → rebuild → replay the input
    (spec §8.3, row "CAS conflict").
    """


class JournalIntegrityError(RuntimeError):
    """The journal contents contradict themselves or the caller's claim.

    A broken digest chain, a `step_seq` that does not follow its predecessor, a checkpoint whose
    `covered_head` does not match the record at its `through_step_seq`. Never retryable — retrying
    replays the same contradiction.
    """


class JournalIoError(RuntimeError):
    """The storage layer failed (disk full, permission denied, hard links unsupported).

    Distinct from both of the above: the journal state is *unknown*, not known-conflicting and not
    known-corrupt. The originating `OSError` is always attached as `__cause__`.
    """


# ------------------------------------------------------------------ #
# Record shapes
# ------------------------------------------------------------------ #


@dataclass
class JournalRecordInput:
    """What a host hands the journal: core's opaque bytes plus the identity core assigned them."""

    #: Chain position. Genesis (`ConfigureOperation`) is 0; every later record is `head + 1`.
    step_seq: int
    #: core's `record_digest`. The journal indexes by it and never recomputes it.
    record_digest: str
    #: core's `record_bytes`, stored verbatim.
    record_bytes: bytes


@dataclass
class JournalEntry(JournalRecordInput):
    """A stored record. `previous_record_digest` is the CAS precondition it was appended under."""

    #: `None` on the genesis record only (spec §8.1: the first append's expected head is empty).
    previous_record_digest: str | None = None


@dataclass
class JournalHead:
    """The journal head: the last record of an operation's chain."""

    step_seq: int
    record_digest: str


@dataclass
class JournalAppendReceipt:
    step_seq: int
    record_digest: str


# ------------------------------------------------------------------ #
# Checkpoint shapes
# ------------------------------------------------------------------ #


@dataclass
class CheckpointCandidate:
    """The host-persistable part of `kernel.checkpoint_candidate()` (spec §12.3)."""

    #: Stable identity of this checkpoint; the CAS token a later install names as its predecessor.
    checkpoint_id: str
    #: The chain position this checkpoint's logical state covers.
    through_step_seq: int
    #: core's digest of the logical state. Opaque to the journal.
    state_digest: str
    #: The serialized checkpoint blob, stored verbatim.
    checkpoint_bytes: bytes


@dataclass
class InstalledCheckpoint(CheckpointCandidate):
    #: Monotonic install ordinal. `previous_checkpoint_id is None` installs ordinal 0.
    ordinal: int
    #: The record digest at `through_step_seq` — verified at install, not required to still be head.
    covered_head: str
    #: Whether `ack_checkpoint` has run. Prefix reclamation is gated on this: a checkpoint that is
    #: installed but not acknowledged is recoverable-from but not prune-authorising (§12.3 rule 5/6).
    acknowledged: bool
    previous_checkpoint_id: str | None = None


@dataclass
class JournalPruneReceipt:
    #: Highest `step_seq` no longer retained. `-1` when nothing has ever been pruned.
    pruned_through_step_seq: int
    pruned_count: int


# ------------------------------------------------------------------ #
# The capability
# ------------------------------------------------------------------ #


class KernelJournal(ABC):
    """The durable transaction capability (spec §9.1).

    Deliberately *not* part of `SessionLog`: a custom business-projection log must never be forced
    to masquerade as a transactional journal (§9.4). One class may hold both — `InMemorySessionLog`
    does — but the interfaces stay separate.

    Guarantees an implementation owes (spec §9.1):

    - strict ordering within an operation;
    - a failed CAS never overwrites;
    - record bytes preserved verbatim;
    - checkpoint pointer advances monotonically;
    - the checkpoint store verifies `covered_head` against `through_step_seq` but does **not**
      require it to still be the current transaction head (§22.14);
    - checkpoint pointer and prefix pruning have an explicit acknowledgement boundary.
    """

    @abstractmethod
    async def compare_and_append(
        self,
        operation_id: str,
        expected_head: str | None,
        record: JournalRecordInput,
    ) -> JournalAppendReceipt:
        """Atomically append `record` iff the operation's head is exactly `expected_head`.

        :param expected_head: `None` starts the chain (genesis; `record.step_seq` must be 0).
        :raises JournalCasConflictError: head moved, or a genesis already exists.
        :raises JournalIntegrityError: `step_seq` does not follow the head.
        :raises JournalIoError: storage failure.
        """

    @abstractmethod
    async def head(self, operation_id: str) -> JournalHead | None:
        """The current head, or `None` when the operation has no records and no pruned anchor."""

    @abstractmethod
    async def read_from(self, operation_id: str, from_step_seq: int = 0) -> list[JournalEntry]:
        """Records with `step_seq >= from_step_seq` (default: the whole retained chain), in order."""

    @abstractmethod
    async def records_after(
        self, operation_id: str, after_head: str | None = None
    ) -> list[JournalEntry]:
        """Records strictly after the record whose digest is `after_head`.

        The digest-anchored cursor §9.1 names `records_after(operation_id, checkpoint_head)`.
        `None` returns everything retained.

        :raises JournalIntegrityError: `after_head` names no retained record and no pruned anchor.
        """

    @abstractmethod
    async def compare_and_install_checkpoint(
        self,
        operation_id: str,
        previous_checkpoint_id: str | None,
        covered_head: str,
        checkpoint: CheckpointCandidate,
    ) -> InstalledCheckpoint:
        """Atomically install `checkpoint` iff the operation's checkpoint pointer is exactly
        `previous_checkpoint_id`, and `covered_head` is the record digest at
        `checkpoint.through_step_seq`.

        Per §22.14 this deliberately does **not** require `covered_head` to still be the current
        transaction head: transactions appended after the candidate was taken stay as tail.

        :param previous_checkpoint_id: `None` installs the operation's first checkpoint.
        :raises JournalCasConflictError: the checkpoint pointer moved.
        :raises JournalIntegrityError: `covered_head`/`through_step_seq` disagree with the chain, or
            `through_step_seq` would move the pointer backwards.
        """

    @abstractmethod
    async def latest_checkpoint(self, operation_id: str) -> InstalledCheckpoint | None:
        """The highest-ordinal installed checkpoint, acknowledged or not."""

    @abstractmethod
    async def ack_checkpoint(self, operation_id: str, checkpoint_id: str) -> InstalledCheckpoint:
        """Record the durable acknowledgement that opens the prefix-reclamation boundary.

        Idempotent.

        :raises JournalIntegrityError: no checkpoint with that id is installed.
        """

    @abstractmethod
    async def prune_acked_prefix(self, operation_id: str) -> JournalPruneReceipt:
        """Reclaim the record prefix covered by the latest **acknowledged** checkpoint.

        A no-op while no checkpoint is acknowledged. The pruned boundary is retained as an anchor so
        `head()` and the next CAS still resolve on a fully-pruned chain.
        """

    @abstractmethod
    async def stage_outbound_envelope(self, operation_id: str, envelope_json: str) -> None:
        """Durably stage one outbound WireEnvelope JSON until append-ack (or abandon).

        Overwrites any previous pending envelope. Required by Phase 6 host cutover
        (adjudication 5e.3): retries must replay byte-identical envelopes.
        """

    @abstractmethod
    async def read_outbound_envelope(self, operation_id: str) -> str | None:
        """Read the staged outbound envelope, if any."""

    @abstractmethod
    async def clear_outbound_envelope(self, operation_id: str) -> None:
        """Clear the staged outbound envelope. Idempotent."""


# ------------------------------------------------------------------ #
# Shared validation
# ------------------------------------------------------------------ #


def _non_negative_safe_integer(value: Any) -> bool:
    return (
        isinstance(value, int)
        and not isinstance(value, bool)
        and 0 <= value <= MAX_SAFE_INTEGER
    )


def _validate_record(record: JournalRecordInput) -> None:
    if not _non_negative_safe_integer(record.step_seq):
        raise JournalIntegrityError("journal record step_seq must be a non-negative safe integer")
    if not record.record_digest:
        raise JournalIntegrityError("journal record requires a record_digest")
    if not isinstance(record.record_bytes, (bytes, bytearray, memoryview)):
        raise JournalIntegrityError("journal record requires opaque record_bytes")


def _validate_candidate(checkpoint: CheckpointCandidate) -> None:
    if not checkpoint.checkpoint_id:
        raise JournalIntegrityError("checkpoint requires a checkpoint_id")
    if not _non_negative_safe_integer(checkpoint.through_step_seq):
        raise JournalIntegrityError("checkpoint through_step_seq must be a non-negative safe integer")
    if not checkpoint.state_digest:
        raise JournalIntegrityError("checkpoint requires a state_digest")
    if not isinstance(checkpoint.checkpoint_bytes, (bytes, bytearray, memoryview)):
        raise JournalIntegrityError("checkpoint requires opaque checkpoint_bytes")


def _check_append_precondition(
    head: JournalHead | None,
    expected_head: str | None,
    record: JournalRecordInput,
) -> None:
    """Check the CAS precondition against the observed head, and the chain position that follows
    from it. Shared by both implementations so their conflict/integrity taxonomy cannot drift apart.
    """
    if expected_head is None:
        if head is not None:
            raise JournalCasConflictError(
                "journal genesis append requires an empty chain, but the operation already has a head"
            )
        if record.step_seq != 0:
            raise JournalIntegrityError("journal genesis record must have step_seq 0")
        return
    if head is None:
        raise JournalCasConflictError(
            "journal compare-and-append expected a head, but the chain is empty"
        )
    if head.record_digest != expected_head:
        raise JournalCasConflictError("journal head changed before compare-and-append")
    if record.step_seq != head.step_seq + 1:
        raise JournalIntegrityError(
            f"journal record step_seq {record.step_seq} does not follow head step_seq {head.step_seq}"
        )


def _verify_chain(entries: list[JournalEntry]) -> None:
    """Verify one contiguous run of entries links head-to-tail."""
    for index in range(1, len(entries)):
        previous = entries[index - 1]
        entry = entries[index]
        if entry.step_seq != previous.step_seq + 1:
            raise JournalIntegrityError(
                f"journal chain has a gap: step_seq {entry.step_seq} follows {previous.step_seq}"
            )
        if entry.previous_record_digest != previous.record_digest:
            raise JournalIntegrityError("journal chain digest linkage is not continuous")


@dataclass
class _PrunedAnchor:
    through_step_seq: int
    covered_head: str


def _check_checkpoint_precondition(
    latest: InstalledCheckpoint | None,
    previous_checkpoint_id: str | None,
    checkpoint: CheckpointCandidate,
) -> None:
    if previous_checkpoint_id is None:
        if latest is not None:
            raise JournalCasConflictError(
                "checkpoint install without a predecessor requires an empty checkpoint pointer"
            )
        return
    if latest is None:
        raise JournalCasConflictError(
            "checkpoint install named a predecessor, but none is installed"
        )
    if latest.checkpoint_id != previous_checkpoint_id:
        raise JournalCasConflictError("checkpoint pointer changed before compare-and-install")
    if checkpoint.through_step_seq < latest.through_step_seq:
        raise JournalIntegrityError("checkpoint pointer must advance monotonically")


def _verify_covered_head(
    covered: JournalEntry | None,
    pruned: _PrunedAnchor | None,
    covered_head: str,
    through_step_seq: int,
) -> None:
    """§9.1: the store verifies that `covered_head` is the record digest at `through_step_seq`.

    It does NOT check that this is still the current head — §22.14 rejects that, since it would
    serialise checkpointing against ordinary transitions.
    """
    if covered is not None:
        if covered.record_digest != covered_head:
            raise JournalIntegrityError(
                "checkpoint covered_head does not match the record at its through_step_seq"
            )
        return
    if (
        pruned is not None
        and pruned.through_step_seq == through_step_seq
        and pruned.covered_head == covered_head
    ):
        return
    raise JournalIntegrityError("checkpoint through_step_seq names no retained record")


# ------------------------------------------------------------------ #
# In-memory implementation
# ------------------------------------------------------------------ #


@dataclass
class _OperationState:
    records: list[JournalEntry]
    checkpoints: list[InstalledCheckpoint]
    pruned: _PrunedAnchor | None = None
    #: Byte-identical WireEnvelope JSON awaiting append-ack (at most one per operation).
    outbound_envelope: str | None = None


class InMemoryKernelJournal(KernelJournal):
    """**Single-process dev/test implementation.**

    CAS is genuinely atomic here — every check-then-mutate below runs to completion without an
    intervening `await`, so no other task can interleave on one asyncio event loop — but that
    atomicity ends at the process boundary (and at the event-loop boundary: it is not thread-safe).
    Two processes sharing "the same" journal do not exist: each has its own dict. Production hosts
    must supply a `KernelJournal` whose CAS is a real storage-layer primitive (spec §9.1);
    `FileKernelJournal` is the reference for that.
    """

    def __init__(self) -> None:
        self._operations: dict[str, _OperationState] = {}

    def _state(self, operation_id: str) -> _OperationState:
        state = self._operations.get(operation_id)
        if state is None:
            state = _OperationState(records=[], checkpoints=[])
            self._operations[operation_id] = state
        return state

    @staticmethod
    def _head_of(state: _OperationState) -> JournalHead | None:
        if state.records:
            last = state.records[-1]
            return JournalHead(step_seq=last.step_seq, record_digest=last.record_digest)
        if state.pruned is not None:
            return JournalHead(
                step_seq=state.pruned.through_step_seq,
                record_digest=state.pruned.covered_head,
            )
        return None

    async def compare_and_append(
        self,
        operation_id: str,
        expected_head: str | None,
        record: JournalRecordInput,
    ) -> JournalAppendReceipt:
        _validate_record(record)
        # Atomic region: no `await` from here to the append.
        state = self._state(operation_id)
        _check_append_precondition(self._head_of(state), expected_head, record)
        state.records.append(
            JournalEntry(
                step_seq=record.step_seq,
                record_digest=record.record_digest,
                record_bytes=bytes(record.record_bytes),
                previous_record_digest=expected_head,
            )
        )
        return JournalAppendReceipt(step_seq=record.step_seq, record_digest=record.record_digest)

    async def head(self, operation_id: str) -> JournalHead | None:
        state = self._operations.get(operation_id)
        return self._head_of(state) if state is not None else None

    async def read_from(self, operation_id: str, from_step_seq: int = 0) -> list[JournalEntry]:
        state = self._operations.get(operation_id)
        records = state.records if state is not None else []
        # Retained records are dense and ordered, so the cursor is an index — not a scan. Callers hit
        # this once per durable step to fetch the head; a filter here would make a run quadratic.
        base = records[0].step_seq if records else 0
        entries = records[max(0, from_step_seq - base) :]
        _verify_chain(entries)
        return [replace(entry) for entry in entries]

    async def records_after(
        self, operation_id: str, after_head: str | None = None
    ) -> list[JournalEntry]:
        if after_head is None:
            return await self.read_from(operation_id, 0)
        state = self._operations.get(operation_id)
        if state is not None:
            anchor = next(
                (entry for entry in state.records if entry.record_digest == after_head), None
            )
            if anchor is not None:
                return await self.read_from(operation_id, anchor.step_seq + 1)
            if state.pruned is not None and state.pruned.covered_head == after_head:
                return await self.read_from(operation_id, state.pruned.through_step_seq + 1)
        raise JournalIntegrityError("journal cursor digest names no retained record")

    async def compare_and_install_checkpoint(
        self,
        operation_id: str,
        previous_checkpoint_id: str | None,
        covered_head: str,
        checkpoint: CheckpointCandidate,
    ) -> InstalledCheckpoint:
        _validate_candidate(checkpoint)
        # Atomic region: no `await` from here to the append.
        state = self._state(operation_id)
        latest = state.checkpoints[-1] if state.checkpoints else None
        _check_checkpoint_precondition(latest, previous_checkpoint_id, checkpoint)
        _verify_covered_head(
            next(
                (
                    entry
                    for entry in state.records
                    if entry.step_seq == checkpoint.through_step_seq
                ),
                None,
            ),
            state.pruned,
            covered_head,
            checkpoint.through_step_seq,
        )
        installed = InstalledCheckpoint(
            checkpoint_id=checkpoint.checkpoint_id,
            through_step_seq=checkpoint.through_step_seq,
            state_digest=checkpoint.state_digest,
            checkpoint_bytes=bytes(checkpoint.checkpoint_bytes),
            ordinal=latest.ordinal + 1 if latest is not None else 0,
            covered_head=covered_head,
            acknowledged=False,
            previous_checkpoint_id=previous_checkpoint_id,
        )
        state.checkpoints.append(installed)
        return replace(installed)

    async def latest_checkpoint(self, operation_id: str) -> InstalledCheckpoint | None:
        state = self._operations.get(operation_id)
        if state is None or not state.checkpoints:
            return None
        return replace(state.checkpoints[-1])

    async def ack_checkpoint(self, operation_id: str, checkpoint_id: str) -> InstalledCheckpoint:
        state = self._operations.get(operation_id)
        installed = (
            next(
                (
                    entry
                    for entry in state.checkpoints
                    if entry.checkpoint_id == checkpoint_id
                ),
                None,
            )
            if state is not None
            else None
        )
        if installed is None:
            raise JournalIntegrityError("cannot acknowledge an uninstalled checkpoint")
        installed.acknowledged = True
        return replace(installed)

    async def prune_acked_prefix(self, operation_id: str) -> JournalPruneReceipt:
        state = self._operations.get(operation_id)
        acked = (
            next(
                (entry for entry in reversed(state.checkpoints) if entry.acknowledged),
                None,
            )
            if state is not None
            else None
        )
        if state is None or acked is None:
            existing = state.pruned if state is not None else None
            return JournalPruneReceipt(
                pruned_through_step_seq=existing.through_step_seq if existing else -1,
                pruned_count=0,
            )
        before = len(state.records)
        state.records = [
            entry for entry in state.records if entry.step_seq > acked.through_step_seq
        ]
        if state.pruned is None or state.pruned.through_step_seq < acked.through_step_seq:
            state.pruned = _PrunedAnchor(
                through_step_seq=acked.through_step_seq, covered_head=acked.covered_head
            )
        return JournalPruneReceipt(
            pruned_through_step_seq=state.pruned.through_step_seq,
            pruned_count=before - len(state.records),
        )

    async def stage_outbound_envelope(self, operation_id: str, envelope_json: str) -> None:
        if not envelope_json:
            raise JournalIntegrityError("outbound envelope must not be empty")
        self._state(operation_id).outbound_envelope = envelope_json

    async def read_outbound_envelope(self, operation_id: str) -> str | None:
        state = self._operations.get(operation_id)
        return None if state is None else state.outbound_envelope

    async def clear_outbound_envelope(self, operation_id: str) -> None:
        state = self._operations.get(operation_id)
        if state is not None:
            state.outbound_envelope = None


# ------------------------------------------------------------------ #
# File implementation
# ------------------------------------------------------------------ #

SEQ_DIGITS = 12
_RECORD_NAME = re.compile(r"^(\d{12})\.rec$")
_CHECKPOINT_NAME = re.compile(r"^(\d{12})\.ckpt$")
_ACK_NAME = re.compile(r"^(\d{12})\.ack$")
_UNSAFE_SEGMENT_CHAR = re.compile(r"[^A-Za-z0-9._-]")


def _pad(value: int) -> str:
    return str(value).zfill(SEQ_DIGITS)


def _escape_char(match: re.Match[str]) -> str:
    """Escape one character as its UTF-16 code units, exactly as the Node port does.

    Node's `charCodeAt` yields UTF-16 units, so an astral character becomes two escapes there.
    Matching that here keeps a journal root byte-identical across the two SDKs.
    """
    encoded = match.group(0).encode("utf-16-le")
    units = struct.unpack(f"<{len(encoded) // 2}H", encoded)
    return "".join(f"~{unit:x}" for unit in units)


def _safe_segment(value: str) -> str:
    """Filesystem-safe, injective encoding of an operation id."""
    return _UNSAFE_SEGMENT_CHAR.sub(_escape_char, value)


def _unlink_quietly(path: Path) -> None:
    try:
        path.unlink()
    except OSError:
        pass


class FileKernelJournal(KernelJournal):
    """**Cross-process atomic reference implementation** of `KernelJournal` (spec Task 8b).

    The atomicity primitive is POSIX `link(2)`: content is written to a private temp file and
    fsynced, then hard-linked into its final name. `link` fails with `EEXIST` if the name is taken,
    and it publishes already-complete content — so a crash can never leave a half-written `.rec`,
    only an orphan temp file that the naming rule ignores. The journal root must therefore live on a
    filesystem that supports hard links; that requirement is the price of real CAS.

    **Why the record filename is `<step_seq>.rec` and contains no digest.** The filename *is* the
    collision domain. Two writers racing on the same head both compute the same next `step_seq`, so
    they contend for one name and exactly one wins. Folding a per-writer value (the new record's
    digest) into the name would give the racers *different* names — both `link`s would succeed and
    the chain would fork. Only the predecessor-determined part of the identity may appear in the
    name.

    The pre-`link` head check is not a TOCTOU hole: it can only *reject* an append that `link` would
    have accepted (a stale `expected_head` whose `step_seq` slot happens to be free), never accept
    one `link` would have rejected. Every acceptance is still decided by the atomic `link`.

    Checkpoint installs use the same primitive on a separate ordinal space (`<ordinal>.ckpt`), so
    two processes installing on the same predecessor also contend for one name.

    File I/O here is synchronous inside `async def` methods, matching `FileSessionLog`: the
    durability barrier is `fsync`, and deferring it to a thread would not make it any more atomic.
    """

    def __init__(self, root: str | Path) -> None:
        self._root = Path(root)

    def _operation_dir(self, operation_id: str) -> Path:
        return self._root / _safe_segment(operation_id)

    def _records_dir(self, operation_id: str) -> Path:
        return self._operation_dir(operation_id) / "records"

    def _checkpoints_dir(self, operation_id: str) -> Path:
        return self._operation_dir(operation_id) / "checkpoints"

    def _tmp_dir(self, operation_id: str) -> Path:
        return self._operation_dir(operation_id) / "tmp"

    def _pruned_path(self, operation_id: str) -> Path:
        return self._operation_dir(operation_id) / "pruned.json"

    def _publish(self, operation_id: str, target: Path, payload: str) -> bool:
        """Write `payload` to a temp file, fsync it, then atomically claim `target` by hard link.

        :returns: `False` when the name was already taken — i.e. a lost CAS race.
        """
        tmp_dir = self._tmp_dir(operation_id)
        tmp_path = tmp_dir / f"{uuid.uuid4().hex}.tmp"
        try:
            tmp_dir.mkdir(parents=True, exist_ok=True)
            descriptor = os.open(tmp_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                os.write(descriptor, payload.encode("utf-8"))
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        except OSError as err:
            raise JournalIoError("journal could not stage a durable record") from err
        try:
            os.link(tmp_path, target)
        except FileExistsError:
            _unlink_quietly(tmp_path)
            return False
        except OSError as err:
            _unlink_quietly(tmp_path)
            raise JournalIoError("journal could not publish a durable record") from err
        _unlink_quietly(tmp_path)
        self._sync_dir(target.parent)
        return True

    @staticmethod
    def _sync_dir(directory: Path) -> None:
        try:
            descriptor = os.open(directory, os.O_RDONLY)
            try:
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
        except OSError:
            # Directory fsync is a durability nicety; some platforms refuse it. Never fail the append.
            pass

    def _record_seqs(self, operation_id: str) -> list[int]:
        """Sorted `step_seq` values of the retained records. Anything not matching the rule is residue."""
        try:
            names = os.listdir(self._records_dir(operation_id))
        except FileNotFoundError:
            return []
        except OSError as err:
            raise JournalIoError("journal could not list its records") from err
        seqs = [int(match.group(1)) for match in map(_RECORD_NAME.match, names) if match]
        return sorted(seqs)

    def _read_record(self, operation_id: str, step_seq: int) -> JournalEntry | None:
        path = self._records_dir(operation_id) / f"{_pad(step_seq)}.rec"
        try:
            raw = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return None
        except OSError as err:
            raise JournalIoError("journal could not read a record") from err
        try:
            persisted = json.loads(raw)
        except ValueError as err:
            raise JournalIntegrityError(
                f"journal record {_pad(step_seq)} is not readable: {err}"
            ) from err
        if persisted.get("step_seq") != step_seq:
            raise JournalIntegrityError(
                f"journal record {_pad(step_seq)} disagrees with its own step_seq"
            )
        return JournalEntry(
            step_seq=persisted["step_seq"],
            record_digest=persisted["record_digest"],
            record_bytes=base64.b64decode(persisted["record_bytes"]),
            previous_record_digest=persisted.get("previous_record_digest"),
        )

    def _pruned_anchor(self, operation_id: str) -> _PrunedAnchor | None:
        try:
            raw = self._pruned_path(operation_id).read_text(encoding="utf-8")
        except FileNotFoundError:
            return None
        except OSError as err:
            raise JournalIoError("journal could not read its pruned anchor") from err
        try:
            persisted = json.loads(raw)
        except ValueError as err:
            raise JournalIoError("journal could not read its pruned anchor") from err
        return _PrunedAnchor(
            through_step_seq=persisted["through_step_seq"],
            covered_head=persisted["covered_head"],
        )

    async def head(self, operation_id: str) -> JournalHead | None:
        seqs = self._record_seqs(operation_id)
        if seqs:
            entry = self._read_record(operation_id, seqs[-1])
            if entry is not None:
                return JournalHead(step_seq=entry.step_seq, record_digest=entry.record_digest)
        pruned = self._pruned_anchor(operation_id)
        if pruned is None:
            return None
        return JournalHead(
            step_seq=pruned.through_step_seq, record_digest=pruned.covered_head
        )

    async def compare_and_append(
        self,
        operation_id: str,
        expected_head: str | None,
        record: JournalRecordInput,
    ) -> JournalAppendReceipt:
        _validate_record(record)
        _check_append_precondition(await self.head(operation_id), expected_head, record)

        records_dir = self._records_dir(operation_id)
        try:
            records_dir.mkdir(parents=True, exist_ok=True)
        except OSError as err:
            raise JournalIoError("journal could not create its record directory") from err
        # Key order mirrors the Node port so both SDKs emit byte-identical record files.
        persisted: dict[str, Any] = {
            "step_seq": record.step_seq,
            "record_digest": record.record_digest,
        }
        if expected_head is not None:
            persisted["previous_record_digest"] = expected_head
        persisted["record_bytes"] = base64.b64encode(bytes(record.record_bytes)).decode("ascii")
        won = self._publish(
            operation_id,
            records_dir / f"{_pad(record.step_seq)}.rec",
            json.dumps(persisted, ensure_ascii=False, separators=(",", ":")),
        )
        if not won:
            raise JournalCasConflictError(
                f"journal step_seq {record.step_seq} was claimed by a concurrent writer"
            )
        return JournalAppendReceipt(step_seq=record.step_seq, record_digest=record.record_digest)

    async def read_from(self, operation_id: str, from_step_seq: int = 0) -> list[JournalEntry]:
        entries: list[JournalEntry] = []
        for seq in self._record_seqs(operation_id):
            if seq < from_step_seq:
                continue
            entry = self._read_record(operation_id, seq)
            if entry is not None:
                entries.append(entry)
        _verify_chain(entries)
        return entries

    async def records_after(
        self, operation_id: str, after_head: str | None = None
    ) -> list[JournalEntry]:
        if after_head is None:
            return await self.read_from(operation_id, 0)
        for seq in self._record_seqs(operation_id):
            entry = self._read_record(operation_id, seq)
            if entry is not None and entry.record_digest == after_head:
                return await self.read_from(operation_id, seq + 1)
        pruned = self._pruned_anchor(operation_id)
        if pruned is not None and pruned.covered_head == after_head:
            return await self.read_from(operation_id, pruned.through_step_seq + 1)
        raise JournalIntegrityError("journal cursor digest names no retained record")

    def _checkpoint_ordinals(self, operation_id: str) -> list[int]:
        """Sorted ordinals of installed checkpoints."""
        try:
            names = os.listdir(self._checkpoints_dir(operation_id))
        except FileNotFoundError:
            return []
        except OSError as err:
            raise JournalIoError("journal could not list its checkpoints") from err
        return sorted(int(match.group(1)) for match in map(_CHECKPOINT_NAME.match, names) if match)

    def _acked_ordinals(self, operation_id: str) -> set[int]:
        try:
            names = os.listdir(self._checkpoints_dir(operation_id))
        except FileNotFoundError:
            return set()
        except OSError as err:
            raise JournalIoError("journal could not list its checkpoints") from err
        return {int(match.group(1)) for match in map(_ACK_NAME.match, names) if match}

    def _read_checkpoint(
        self,
        operation_id: str,
        ordinal: int,
        acked: set[int] | None = None,
    ) -> InstalledCheckpoint | None:
        path = self._checkpoints_dir(operation_id) / f"{_pad(ordinal)}.ckpt"
        try:
            raw = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return None
        except OSError as err:
            raise JournalIoError("journal could not read a checkpoint") from err
        try:
            persisted = json.loads(raw)
        except ValueError as err:
            raise JournalIntegrityError(
                f"checkpoint {_pad(ordinal)} is not readable: {err}"
            ) from err
        acknowledged = ordinal in (
            acked if acked is not None else self._acked_ordinals(operation_id)
        )
        return InstalledCheckpoint(
            checkpoint_id=persisted["checkpoint_id"],
            through_step_seq=persisted["through_step_seq"],
            state_digest=persisted["state_digest"],
            checkpoint_bytes=base64.b64decode(persisted["checkpoint_bytes"]),
            ordinal=persisted["ordinal"],
            covered_head=persisted["covered_head"],
            acknowledged=acknowledged,
            previous_checkpoint_id=persisted.get("previous_checkpoint_id"),
        )

    async def latest_checkpoint(self, operation_id: str) -> InstalledCheckpoint | None:
        ordinals = self._checkpoint_ordinals(operation_id)
        if not ordinals:
            return None
        return self._read_checkpoint(operation_id, ordinals[-1])

    async def compare_and_install_checkpoint(
        self,
        operation_id: str,
        previous_checkpoint_id: str | None,
        covered_head: str,
        checkpoint: CheckpointCandidate,
    ) -> InstalledCheckpoint:
        _validate_candidate(checkpoint)
        latest = await self.latest_checkpoint(operation_id)
        _check_checkpoint_precondition(latest, previous_checkpoint_id, checkpoint)
        _verify_covered_head(
            self._read_record(operation_id, checkpoint.through_step_seq),
            self._pruned_anchor(operation_id),
            covered_head,
            checkpoint.through_step_seq,
        )

        checkpoints_dir = self._checkpoints_dir(operation_id)
        try:
            checkpoints_dir.mkdir(parents=True, exist_ok=True)
        except OSError as err:
            raise JournalIoError("journal could not create its checkpoint directory") from err
        ordinal = latest.ordinal + 1 if latest is not None else 0
        persisted: dict[str, Any] = {
            "ordinal": ordinal,
            "checkpoint_id": checkpoint.checkpoint_id,
        }
        if previous_checkpoint_id is not None:
            persisted["previous_checkpoint_id"] = previous_checkpoint_id
        persisted.update(
            covered_head=covered_head,
            through_step_seq=checkpoint.through_step_seq,
            state_digest=checkpoint.state_digest,
            checkpoint_bytes=base64.b64encode(bytes(checkpoint.checkpoint_bytes)).decode("ascii"),
        )
        won = self._publish(
            operation_id,
            checkpoints_dir / f"{_pad(ordinal)}.ckpt",
            json.dumps(persisted, ensure_ascii=False, separators=(",", ":")),
        )
        if not won:
            raise JournalCasConflictError(
                f"checkpoint ordinal {ordinal} was claimed by a concurrent installer"
            )
        return InstalledCheckpoint(
            checkpoint_id=checkpoint.checkpoint_id,
            through_step_seq=checkpoint.through_step_seq,
            state_digest=checkpoint.state_digest,
            checkpoint_bytes=bytes(checkpoint.checkpoint_bytes),
            ordinal=ordinal,
            covered_head=covered_head,
            acknowledged=False,
            previous_checkpoint_id=previous_checkpoint_id,
        )

    async def ack_checkpoint(self, operation_id: str, checkpoint_id: str) -> InstalledCheckpoint:
        for ordinal in reversed(self._checkpoint_ordinals(operation_id)):
            installed = self._read_checkpoint(operation_id, ordinal)
            if installed is None or installed.checkpoint_id != checkpoint_id:
                continue
            if not installed.acknowledged:
                # `_publish` returning False means another process already acknowledged it — idempotent.
                self._publish(
                    operation_id,
                    self._checkpoints_dir(operation_id) / f"{_pad(ordinal)}.ack",
                    json.dumps(
                        {"ordinal": ordinal, "checkpoint_id": checkpoint_id},
                        ensure_ascii=False,
                        separators=(",", ":"),
                    ),
                )
            return replace(installed, acknowledged=True)
        raise JournalIntegrityError("cannot acknowledge an uninstalled checkpoint")

    async def prune_acked_prefix(self, operation_id: str) -> JournalPruneReceipt:
        acked = self._acked_ordinals(operation_id)
        existing = self._pruned_anchor(operation_id)
        boundary: InstalledCheckpoint | None = None
        for ordinal in reversed(self._checkpoint_ordinals(operation_id)):
            if ordinal not in acked:
                continue
            boundary = self._read_checkpoint(operation_id, ordinal, acked)
            break
        if boundary is None:
            return JournalPruneReceipt(
                pruned_through_step_seq=existing.through_step_seq if existing else -1,
                pruned_count=0,
            )
        # Anchor first, delete second: a crash mid-prune leaves a resolvable head either way.
        if existing is None or existing.through_step_seq < boundary.through_step_seq:
            self._write_anchor(
                operation_id,
                _PrunedAnchor(
                    through_step_seq=boundary.through_step_seq,
                    covered_head=boundary.covered_head,
                ),
            )
        pruned = 0
        for seq in self._record_seqs(operation_id):
            if seq > boundary.through_step_seq:
                break
            _unlink_quietly(self._records_dir(operation_id) / f"{_pad(seq)}.rec")
            pruned += 1
        return JournalPruneReceipt(
            pruned_through_step_seq=max(
                boundary.through_step_seq, existing.through_step_seq if existing else -1
            ),
            pruned_count=pruned,
        )

    def _write_anchor(self, operation_id: str, anchor: _PrunedAnchor) -> None:
        """The anchor only ever moves forward, so an atomic overwriting rename is the right primitive."""
        self._write_replaceable(
            operation_id,
            self._pruned_path(operation_id),
            json.dumps(
                {
                    "through_step_seq": anchor.through_step_seq,
                    "covered_head": anchor.covered_head,
                },
                ensure_ascii=False,
                separators=(",", ":"),
            ),
            "journal could not record its pruned anchor",
        )

    def _outbound_path(self, operation_id: str) -> Path:
        return self._operation_dir(operation_id) / "outbound.json"

    def _write_replaceable(
        self,
        operation_id: str,
        target: Path,
        payload: str,
        error_message: str,
    ) -> None:
        """Overwriteable durable blob — write-tmp → fsync → rename (same as the pruned anchor)."""
        tmp_dir = self._tmp_dir(operation_id)
        tmp_path = tmp_dir / f"{uuid.uuid4().hex}.tmp"
        try:
            tmp_dir.mkdir(parents=True, exist_ok=True)
            descriptor = os.open(tmp_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                os.write(descriptor, payload.encode("utf-8"))
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            os.replace(tmp_path, target)
        except OSError as err:
            _unlink_quietly(tmp_path)
            raise JournalIoError(error_message) from err

    async def stage_outbound_envelope(self, operation_id: str, envelope_json: str) -> None:
        if not envelope_json:
            raise JournalIntegrityError("outbound envelope must not be empty")
        try:
            self._operation_dir(operation_id).mkdir(parents=True, exist_ok=True)
        except OSError as err:
            raise JournalIoError("journal could not create its operation directory") from err
        self._write_replaceable(
            operation_id,
            self._outbound_path(operation_id),
            envelope_json,
            "journal could not stage a durable outbound envelope",
        )

    async def read_outbound_envelope(self, operation_id: str) -> str | None:
        path = self._outbound_path(operation_id)
        try:
            return path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return None
        except OSError as err:
            raise JournalIoError("journal could not read its outbound envelope") from err

    async def clear_outbound_envelope(self, operation_id: str) -> None:
        path = self._outbound_path(operation_id)
        try:
            path.unlink()
        except FileNotFoundError:
            return
        except OSError as err:
            raise JournalIoError("journal could not clear its outbound envelope") from err
