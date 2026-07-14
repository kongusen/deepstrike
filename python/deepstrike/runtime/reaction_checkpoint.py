from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Callable, Literal, Protocol


@dataclass(frozen=True)
class ReactionRecord:
    persona_id: str
    output: str


@dataclass(frozen=True)
class ReactionCheckpointReceipt:
    checkpoint_key: str
    lease_token: str


@dataclass(frozen=True)
class ReactionCheckpointClaim(ReactionCheckpointReceipt):
    lease_expires_at_ms: int
    plan: list[str] | None = None
    outputs: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class ReactionCheckpointClaimResult:
    status: Literal["claimed", "completed", "busy"]
    claim: ReactionCheckpointClaim | None = None
    reactions: list[ReactionRecord] = field(default_factory=list)


class ReactionCheckpointStore(Protocol):
    async def claim(self, checkpoint_key: str, lease_ms: int | None = None) -> ReactionCheckpointClaimResult: ...
    async def save_plan(self, receipt: ReactionCheckpointReceipt, persona_ids: list[str]) -> list[str] | None: ...
    async def record(self, receipt: ReactionCheckpointReceipt, reaction: ReactionRecord) -> bool: ...
    async def complete(self, receipt: ReactionCheckpointReceipt) -> bool: ...
    async def release(self, receipt: ReactionCheckpointReceipt) -> bool: ...


@dataclass
class _CheckpointState:
    plan: list[str] | None = None
    outputs: dict[str, str] = field(default_factory=dict)
    completed: bool = False
    lease_token: str | None = None
    lease_expires_at_ms: int | None = None


class InMemoryReactionCheckpointStore:
    """Process-local reference implementation; durable stores implement the same atomic contract."""
    def __init__(self, *, now: Callable[[], int] | None = None, default_lease_ms: int = 900_000) -> None:
        if default_lease_ms <= 0:
            raise ValueError("default_lease_ms must be positive")
        self._now = now or (lambda: int(time.time() * 1000))
        self._default_lease_ms = default_lease_ms
        self._states: dict[str, _CheckpointState] = {}
        self._lease_seq = 0

    async def claim(self, checkpoint_key: str, lease_ms: int | None = None) -> ReactionCheckpointClaimResult:
        duration = self._default_lease_ms if lease_ms is None else lease_ms
        if duration <= 0:
            raise ValueError("lease_ms must be positive")
        now = self._now()
        state = self._states.setdefault(checkpoint_key, _CheckpointState())
        if state.completed:
            return ReactionCheckpointClaimResult("completed", reactions=self._reactions(state))
        if state.lease_token is not None and (state.lease_expires_at_ms or 0) > now:
            return ReactionCheckpointClaimResult("busy")
        self._lease_seq += 1
        token = f"{checkpoint_key}:lease-{self._lease_seq}"
        expires_at = now + duration
        state.lease_token = token
        state.lease_expires_at_ms = expires_at
        return ReactionCheckpointClaimResult("claimed", claim=ReactionCheckpointClaim(
            checkpoint_key, token, expires_at,
            list(state.plan) if state.plan is not None else None,
            dict(state.outputs),
        ))

    async def save_plan(self, receipt: ReactionCheckpointReceipt, persona_ids: list[str]) -> list[str] | None:
        state = self._current(receipt)
        if state is None:
            return None
        if state.plan is None:
            state.plan = list(dict.fromkeys(persona_ids))
        return list(state.plan)

    async def record(self, receipt: ReactionCheckpointReceipt, reaction: ReactionRecord) -> bool:
        state = self._current(receipt)
        if state is None:
            return False
        state.outputs[reaction.persona_id] = reaction.output
        return True

    async def complete(self, receipt: ReactionCheckpointReceipt) -> bool:
        state = self._current(receipt)
        if state is None:
            return False
        if state.plan is None or any(persona_id not in state.outputs for persona_id in state.plan):
            raise RuntimeError("cannot complete a reaction checkpoint with unfinished personas")
        state.completed = True
        state.lease_token = None
        state.lease_expires_at_ms = None
        return True

    async def release(self, receipt: ReactionCheckpointReceipt) -> bool:
        state = self._current(receipt)
        if state is None:
            return False
        state.lease_token = None
        state.lease_expires_at_ms = None
        return True

    def _current(self, receipt: ReactionCheckpointReceipt) -> _CheckpointState | None:
        state = self._states.get(receipt.checkpoint_key)
        return state if state is not None and state.lease_token == receipt.lease_token else None

    @staticmethod
    def _reactions(state: _CheckpointState) -> list[ReactionRecord]:
        return [ReactionRecord(persona_id, state.outputs[persona_id]) for persona_id in (state.plan or [])]


class ReactionInProgressError(RuntimeError):
    def __init__(self, checkpoint_key: str) -> None:
        self.checkpoint_key = checkpoint_key
        super().__init__(f"reaction checkpoint is already in progress: {checkpoint_key}")
