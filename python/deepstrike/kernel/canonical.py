"""Language-idiomatic projection of the native Canonical Kernel ABI binding."""

from __future__ import annotations

from dataclasses import dataclass
import json
from typing import Literal, TypeAlias, TypeVar

from deepstrike._kernel import KERNEL_ABI_VERSION, _CanonicalKernel as _NativeCanonicalKernel


@dataclass(frozen=True, slots=True)
class CanonicalPrepared:
  status: Literal["prepared"]
  prepare_token: str
  step_seq: int
  expected_head: str | None
  record_digest: str
  record_bytes: bytes
  planned_step_json: str


@dataclass(frozen=True, slots=True)
class CanonicalReplayed:
  status: Literal["replayed"]
  step_seq: int
  expected_head: str | None
  record_digest: str
  record_bytes: bytes | None
  planned_step_json: str | None


@dataclass(frozen=True, slots=True)
class CanonicalRejected:
  status: Literal["rejected"]
  code: str
  message: str


CanonicalPreparation: TypeAlias = CanonicalPrepared | CanonicalReplayed | CanonicalRejected
CanonicalLifecycle: TypeAlias = Literal[
  "created",
  "configured",
  "running",
  "suspended",
  "completed",
  "cancelled",
  "failed",
]


@dataclass(frozen=True, slots=True)
class CanonicalCommit:
  step_seq: int
  record_digest: str
  planned_step_json: str
  checkpoint_advice_json: str | None


@dataclass(frozen=True, slots=True)
class CanonicalCheckpoint:
  checkpoint_bytes: bytes
  through_step_seq: int
  covered_head: str
  state_digest: str
  ack_token: str


@dataclass(frozen=True, slots=True)
class CanonicalRestoreCost:
  records_before_checkpoint: int
  tail_inputs_replayed: int
  records_after_checkpoint: int
  bytes_read: int


class CanonicalKernelError(ValueError):
  """Typed exception projection for a failed commit/abort/checkpoint/restore operation."""

  def __init__(self, code: str, message: str) -> None:
    super().__init__(f"{code}: {message}" if message else code)
    self.code = code
    self.message = message


class CanonicalKernel:
  """Durable canonical operation handle; intentionally has no direct ``step`` method."""

  def __init__(self) -> None:
    self._native = _NativeCanonicalKernel()

  def prepare(self, input_json: str) -> CanonicalPreparation:
    raw = self._native.prepare(input_json)
    if raw.status == "prepared":
      return CanonicalPrepared(
        status="prepared",
        prepare_token=_required(raw.prepare_token, "prepare_token"),
        step_seq=_required(raw.step_seq, "step_seq"),
        expected_head=raw.expected_head,
        record_digest=_required(raw.record_digest, "record_digest"),
        record_bytes=bytes(_required(raw.record_bytes, "record_bytes")),
        planned_step_json=_required(raw.planned_step_json, "planned_step_json"),
      )
    if raw.status == "replayed":
      return CanonicalReplayed(
        status="replayed",
        step_seq=_required(raw.step_seq, "step_seq"),
        expected_head=raw.expected_head,
        record_digest=_required(raw.record_digest, "record_digest"),
        record_bytes=None if raw.record_bytes is None else bytes(raw.record_bytes),
        planned_step_json=raw.planned_step_json,
      )
    if raw.status == "rejected":
      fault = json.loads(_required(raw.fault_json, "fault_json"))
      return CanonicalRejected(
        status="rejected",
        code=str(fault["code"]),
        message=str(fault.get("message", "")),
      )
    raise RuntimeError(f"native canonical binding returned unknown status {raw.status!r}")

  def commit(self, prepare_token: str, appended_head: str) -> CanonicalCommit:
    try:
      raw = self._native.commit(prepare_token, appended_head)
    except ValueError as error:
      raise _project_error(error) from error
    return CanonicalCommit(
      step_seq=raw.step_seq,
      record_digest=raw.record_digest,
      planned_step_json=raw.planned_step_json,
      checkpoint_advice_json=raw.checkpoint_advice_json,
    )

  def abort(self, prepare_token: str) -> None:
    try:
      self._native.abort(prepare_token)
    except ValueError as error:
      raise _project_error(error) from error

  def checkpoint_candidate(self) -> CanonicalCheckpoint:
    try:
      return _checkpoint(self._native.checkpoint_candidate())
    except ValueError as error:
      raise _project_error(error) from error

  def checkpoint_rebase(self, checkpoint_bytes: bytes) -> CanonicalCheckpoint:
    try:
      return _checkpoint(self._native.checkpoint_rebase(checkpoint_bytes))
    except ValueError as error:
      raise _project_error(error) from error

  def ack_checkpoint(self, through_step_seq: int, covered_head: str) -> None:
    try:
      self._native.ack_checkpoint(through_step_seq, covered_head)
    except ValueError as error:
      raise _project_error(error) from error

  def restore(
    self,
    checkpoint_bytes: bytes | None,
    record_bytes: list[bytes],
  ) -> CanonicalRestoreCost:
    """Replace this handle's native state in place."""
    try:
      raw = self._native.restore(checkpoint_bytes, record_bytes)
    except ValueError as error:
      raise _project_error(error) from error
    return CanonicalRestoreCost(
      records_before_checkpoint=raw.records_before_checkpoint,
      tail_inputs_replayed=raw.tail_inputs_replayed,
      records_after_checkpoint=raw.records_after_checkpoint,
      bytes_read=raw.bytes_read,
    )

  def lifecycle(self) -> CanonicalLifecycle:
    return self._native.lifecycle()

  def pending_effects_json(self) -> str:
    return self._native.pending_effects_json()

  def terminal_json(self) -> str | None:
    return self._native.terminal_json()


def _checkpoint(raw: object) -> CanonicalCheckpoint:
  return CanonicalCheckpoint(
    checkpoint_bytes=bytes(getattr(raw, "checkpoint_bytes")),
    through_step_seq=getattr(raw, "through_step_seq"),
    covered_head=getattr(raw, "covered_head"),
    state_digest=getattr(raw, "state_digest"),
    ack_token=getattr(raw, "ack_token"),
  )


_T = TypeVar("_T")


def _required(value: _T | None, field: str) -> _T:
  if value is None:
    raise RuntimeError(f"native canonical binding omitted required {field}")
  return value


def _project_error(error: ValueError) -> CanonicalKernelError:
  try:
    fault = json.loads(str(error))
  except json.JSONDecodeError:
    return CanonicalKernelError("binding_error", str(error))
  return CanonicalKernelError(str(fault.get("code", "binding_error")), str(fault.get("message", "")))


__all__ = [
  "KERNEL_ABI_VERSION",
  "CanonicalKernel",
  "CanonicalKernelError",
  "CanonicalPrepared",
  "CanonicalReplayed",
  "CanonicalRejected",
  "CanonicalPreparation",
  "CanonicalLifecycle",
  "CanonicalCommit",
  "CanonicalCheckpoint",
  "CanonicalRestoreCost",
]
