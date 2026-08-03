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
async def test_publishes_committed_transition_even_when_advised_checkpoint_fails():
    from deepstrike.runtime.canonical_kernel_step import CanonicalKernelHost

    class AdvisingKernel(FakeKernel):
        def commit(self, token: str, head: str) -> CanonicalCommit:
            committed = super().commit(token, head)
            return CanonicalCommit(
                step_seq=committed.step_seq,
                record_digest=committed.record_digest,
                planned_step_json=committed.planned_step_json,
                checkpoint_advice_json=json.dumps({"through_step_seq": "0"}),
            )

    class CheckpointIoJournal(InMemoryKernelJournal):
        def __init__(self) -> None:
            super().__init__()
            self.installs = 0

        async def compare_and_install_checkpoint(self, *args, **kwargs):
            self.installs += 1
            if self.installs == 1:
                raise OSError("journal io: storage 503")
            return await super().compare_and_install_checkpoint(*args, **kwargs)

    kernel = AdvisingKernel()
    journal = CheckpointIoJournal()
    host = CanonicalKernelHost(kernel, journal, "op")

    # The record is durable and commit already returned: a failing §12.3 checkpoint
    # is deferred housekeeping (the next advice or the checkpoint_required gate
    # retries it), not a failed commit.
    transition = await host.transition(
        {"kind": "configure_operation", "config": {}},
        input_id="input-checkpoint-io",
        observed_at_ms="1",
    )

    assert transition.replayed is False
    assert transition.checkpoint_advice is not None
    assert "storage 503" in (transition.checkpoint_failure or "")
    # Not misdiagnosed as a lost commit: no rebuild happened.
    assert kernel.restores == []
    # The committed step is durable and the stage is clear for the next input.
    assert (await journal.head("op")).record_digest == "digest-1"
    assert await journal.read_outbound_envelope("op") is None


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


class SequencedFakeKernel:
    """Multi-step fake kernel for CanonicalRunnerRuntime rebuild-recovery tests."""

    def __init__(self) -> None:
        self.next_step = 0
        self.head: str | None = None
        self.restores: list[tuple[bytes | None, list[bytes]]] = []
        self.fail_commit_once = False
        self._lifecycle = "created"
        self.checkpoint_advice_json: str | None = None

    def prepare(self, envelope: str):
        return CanonicalPrepared(
            status="prepared",
            prepare_token=f"prepare-{self.next_step}",
            step_seq=self.next_step,
            expected_head=self.head,
            record_digest=f"digest-{self.next_step}",
            record_bytes=f"record-{self.next_step}".encode(),
            planned_step_json='{"disposition":{"kind":"effects","effects":[]}}',
        )

    def commit(self, token: str, head: str) -> CanonicalCommit:
        if self.fail_commit_once:
            self.fail_commit_once = False
            raise RuntimeError("response lost")
        step = self.next_step
        self.head = head
        self.next_step += 1
        self._lifecycle = "configured" if step == 0 else "running"
        return CanonicalCommit(
            step_seq=step,
            record_digest=head,
            planned_step_json='{"disposition":{"kind":"effects","effects":[]}}',
            checkpoint_advice_json=self.checkpoint_advice_json,
        )

    def abort(self, token: str) -> None:
        return None

    def restore(self, checkpoint: bytes | None, records: list[bytes]) -> None:
        self.restores.append((checkpoint, records))
        self.next_step = len(records)
        self.head = f"digest-{len(records) - 1}" if records else None
        if self.next_step == 0:
            self._lifecycle = "created"
        elif self.next_step == 1:
            self._lifecycle = "configured"
        else:
            self._lifecycle = "running"

    def checkpoint_candidate(self) -> CanonicalCheckpoint:
        return CanonicalCheckpoint(
            b"checkpoint", max(0, self.next_step - 1), self.head or "digest-0", "state", "checkpoint-1",
        )

    def ack_checkpoint(self, through_step_seq: int, covered_head: str) -> None:
        return None

    def lifecycle(self) -> str:
        return self._lifecycle

    def pending_effects_json(self) -> str:
        return "[]"

    def terminal_json(self) -> str | None:
        return None


@pytest.mark.asyncio
async def test_runner_continues_on_rebuilt_kernel_after_lost_commit_response():
    from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime

    kernel = SequencedFakeKernel()
    kernel.fail_commit_once = True
    journal = InMemoryKernelJournal()
    runtime = CanonicalRunnerRuntime(kernel, journal, "op-rebuild", max_context_tokens=8_192)

    # configure is appended, commit response is lost, host rebuilds — runner must continue
    # into start_operation instead of terminating the run.
    await runtime.start_agent({"goal": "recover across a lost commit response"})

    assert kernel.restores  # rebuild happened
    assert len(await journal.read_from("op-rebuild")) == 2
    assert any(obs.get("kind") == "kernel_rebuilt" for obs in runtime.drain_host_observations())


@pytest.mark.asyncio
async def test_runner_surfaces_deferred_checkpoint_failure_as_observation():
    from deepstrike.runtime.canonical_kernel_step import CanonicalRunnerRuntime

    class CheckpointIoJournal(InMemoryKernelJournal):
        def __init__(self) -> None:
            super().__init__()
            self.installs = 0

        async def compare_and_install_checkpoint(self, *args, **kwargs):
            self.installs += 1
            if self.installs == 1:
                raise OSError("journal io: storage 503")
            return await super().compare_and_install_checkpoint(*args, **kwargs)

    kernel = SequencedFakeKernel()
    kernel.checkpoint_advice_json = json.dumps({"through_step_seq": "0"})
    journal = CheckpointIoJournal()
    runtime = CanonicalRunnerRuntime(kernel, journal, "op-deferred", max_context_tokens=8_192)

    await runtime.start_agent({"goal": "observe deferred checkpoint"})

    observations = runtime.drain_host_observations()
    deferred = next((obs for obs in observations if obs.get("kind") == "checkpoint_deferred"), None)
    assert deferred is not None
    assert "storage 503" in str(deferred.get("reason"))


@pytest.mark.asyncio
async def test_runner_still_fails_when_journal_rebuild_itself_fails():
    from deepstrike.runtime.canonical_kernel_step import (
        CanonicalKernelRebuildRequiredError,
        CanonicalRunnerRuntime,
    )

    class UnreadableJournal(InMemoryKernelJournal):
        async def records_after(self, *args, **kwargs):
            raise OSError("journal unreadable")

    kernel = SequencedFakeKernel()
    kernel.fail_commit_once = True
    runtime = CanonicalRunnerRuntime(
        kernel, UnreadableJournal(), "op-fatal", max_context_tokens=8_192,
    )

    with pytest.raises(CanonicalKernelRebuildRequiredError):
        await runtime.start_agent({"goal": "fatal"})
