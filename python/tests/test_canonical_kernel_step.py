from __future__ import annotations

import json
from pathlib import Path

import pytest

from deepstrike.kernel.canonical import (
    CanonicalCheckpoint,
    CanonicalCommit,
    CanonicalPrepared,
)
from deepstrike.runtime.kernel_journal import InMemoryKernelJournal, JournalCasConflictError


class FakeKernel:
    def __init__(self) -> None:
        self.prepares: list[str] = []
        self.commits: list[tuple[str, str]] = []
        self.restores: list[tuple[bytes | None, list[bytes]]] = []
        self.aborts: list[str] = []
        self.fail_commit = False

    def prepare(self, envelope: str):
        self.prepares.append(envelope)
        return CanonicalPrepared(
            status="prepared",
            prepare_token="prepare-1",
            step_seq=0,
            expected_head=None,
            record_digest="digest-1",
            record_bytes=b"record-1",
            planned_step_json='{"disposition":{"kind":"effects","effects":[]}}',
        )

    def commit(self, token: str, head: str) -> CanonicalCommit:
        self.commits.append((token, head))
        if self.fail_commit:
            raise RuntimeError("commit lost")
        return CanonicalCommit(
            step_seq=0,
            record_digest=head,
            planned_step_json='{"disposition":{"kind":"effects","effects":[]}}',
            checkpoint_advice_json=None,
        )

    def abort(self, token: str) -> None:
        self.aborts.append(token)

    def restore(self, checkpoint: bytes | None, records: list[bytes]) -> None:
        self.restores.append((checkpoint, records))

    def checkpoint_candidate(self) -> CanonicalCheckpoint:
        return CanonicalCheckpoint(b"checkpoint", 0, "digest-1", "state", "checkpoint-1")

    def ack_checkpoint(self, through_step_seq: int, covered_head: str) -> None:
        assert (through_step_seq, covered_head) == (0, "digest-1")


@pytest.mark.asyncio
async def test_publishes_only_after_durable_append():
    from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost

    kernel = FakeKernel()
    journal = InMemoryKernelJournal()
    host = CanonicalKernelHost(kernel, journal, "op")

    transition = await host.transition({"kind": "configure_operation", "config": {}})

    assert transition.record_digest == "digest-1"
    assert kernel.commits == [("prepare-1", "digest-1")]
    assert (await journal.head("op")).record_digest == "digest-1"
    assert await journal.read_outbound_envelope("op") is None


@pytest.mark.asyncio
async def test_cas_conflict_restores_and_retries_same_envelope_bytes():
    from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost

    class ConflictingJournal(InMemoryKernelJournal):
        def __init__(self) -> None:
            super().__init__()
            self.calls = 0

        async def compare_and_append(self, *args, **kwargs):
            self.calls += 1
            if self.calls == 1:
                raise JournalCasConflictError("lost race")
            return await super().compare_and_append(*args, **kwargs)

    kernel = FakeKernel()
    journal = ConflictingJournal()
    host = CanonicalKernelHost(kernel, journal, "op")

    await host.transition(
        {"kind": "configure_operation", "config": {}},
        input_id="stable-input",
        observed_at_ms="1",
    )

    assert len(kernel.prepares) == 2
    assert kernel.prepares[0] == kernel.prepares[1]
    assert kernel.restores == [(None, [])]


@pytest.mark.asyncio
async def test_commit_loss_rebuilds_and_clears_staged_envelope():
    from deepstrike.runtime.canonical_kernel_step import (
        CanonicalKernelHost,
        CanonicalKernelRebuildRequiredError,
    )

    kernel = FakeKernel()
    kernel.fail_commit = True
    journal = InMemoryKernelJournal()
    host = CanonicalKernelHost(kernel, journal, "op")

    with pytest.raises(CanonicalKernelRebuildRequiredError):
        await host.transition({"kind": "configure_operation", "config": {}})

    assert kernel.restores == [(None, [b"record-1"])]
    assert await journal.read_outbound_envelope("op") is None


@pytest.mark.asyncio
async def test_checkpoint_installs_acknowledges_core_then_prunes():
    from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost

    kernel = FakeKernel()
    journal = InMemoryKernelJournal()
    host = CanonicalKernelHost(kernel, journal, "op")
    await host.transition({"kind": "configure_operation", "config": {}})

    checkpoint = await host.checkpoint()

    assert checkpoint.acknowledged is True
    assert (await journal.latest_checkpoint("op")).acknowledged is True
    assert await journal.read_from("op") == []


@pytest.mark.asyncio
async def test_staged_outbound_survives_append_failure_and_drain_replays_exact_bytes():
    from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost

    class FailingJournal(InMemoryKernelJournal):
        fail = True

        async def compare_and_append(self, *args, **kwargs):
            if self.fail:
                raise OSError("disk unavailable")
            return await super().compare_and_append(*args, **kwargs)

    kernel = FakeKernel()
    journal = FailingJournal()
    host = CanonicalKernelHost(kernel, journal, "op")
    with pytest.raises(OSError):
        await host.transition(
            {"kind": "configure_operation", "config": {}},
            input_id="stable-input",
            observed_at_ms="1",
        )

    staged = await journal.read_outbound_envelope("op")
    assert staged is not None
    journal.fail = False
    await host.drain_outbound_envelope()

    assert kernel.prepares[-1] == staged
    assert await journal.read_outbound_envelope("op") is None
    assert json.loads(staged)["input_id"] == "stable-input"


def test_unknown_effect_preserves_correlation_and_returns_shared_protocol_error_resolution():
    from deepstrike.runtime.canonical_kernel_step import (
        canonical_action_from_planned_step,
        canonical_unsupported_effect_resolution,
    )

    fixture = json.loads(
        (Path(__file__).parents[2] / "tests/fixtures/abi/unknown_effect_protocol_error.json")
        .read_text(encoding="utf-8")
    )
    action = canonical_action_from_planned_step(fixture["planned_step"])

    assert action is not None
    assert {
        "kind": action.kind,
        "effect_id": action.effect_id,
        "effect_kind": action.effect_kind,
    } == fixture["expected_action"]
    assert canonical_unsupported_effect_resolution(action.effect_id, action.effect_kind) == fixture["expected_resolution"]
