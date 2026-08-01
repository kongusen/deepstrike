"""Canonical kernel host and production-runner projection for Python.

Core prepares opaque bytes, the journal makes them authoritative, and only then can
a planned step become visible to the host.
"""

from __future__ import annotations

import hashlib
import json
import re
import time
import uuid
from dataclasses import dataclass
from types import SimpleNamespace
from typing import Any

from deepstrike._kernel import Message, ToolCall, ToolSchema
from deepstrike.kernel.canonical import (
    KERNEL_ABI_VERSION,
    CanonicalCheckpoint,
    CanonicalKernel,
    CanonicalRejected,
)
from deepstrike.runtime.kernel_journal import (
    CheckpointCandidate,
    InstalledCheckpoint,
    JournalCasConflictError,
    JournalRecordInput,
    KernelJournal,
)
from deepstrike.runtime.kernel_step import (
  KernelRunnerAction,
  _context_from_kernel,
  _message_from_kernel,
  encode_canonical_content_parts,
)


MAX_CHAIN_POSITION = 9_007_199_254_740_991


class CanonicalKernelRejectedError(RuntimeError):
  def __init__(self, code: str, message: str) -> None:
    super().__init__(f"{code}: {message}")
    self.code = code
    self.message = message


class CanonicalKernelRebuildRequiredError(RuntimeError):
  """A record is durable but its commit could not be safely published."""


@dataclass(frozen=True)
class CanonicalTransition:
  input_json: str
  step_seq: int
  record_digest: str
  planned_step: dict[str, Any]
  checkpoint_advice: dict[str, Any] | None
  replayed: bool


def _object(value: Any) -> dict[str, Any]:
  return value if isinstance(value, dict) else {}


def _chain_position(value: Any, label: str) -> int:
  if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value < MAX_CHAIN_POSITION:
    raise ValueError(f"{label} must be a non-negative safe integer below {MAX_CHAIN_POSITION}")
  return value


def _planned_step(raw: str | None) -> dict[str, Any]:
  if not raw:
    raise RuntimeError("canonical replay is missing planned_step_json")
  parsed = json.loads(raw)
  if not isinstance(parsed, dict):
    raise RuntimeError("canonical planned step must be an object")
  return parsed


def canonical_unsupported_effect_resolution(effect_id: str, effect_kind: str) -> dict[str, Any]:
  return {
    "kind": "resolve_effect",
    "effect_id": effect_id,
    "outcome": {
      "status": "failed",
      "failure": {
        "kind": "protocol_error",
        "message": f"unknown canonical effect kind: {effect_kind}",
        "retryable": False,
      },
    },
  }


def _advice(raw: str | None) -> dict[str, Any] | None:
  if not raw:
    return None
  parsed = json.loads(raw)
  return parsed if isinstance(parsed, dict) else None


class CanonicalKernelHost:
  """Durable staged-transition host for :class:`CanonicalKernel`."""

  def __init__(self, kernel: CanonicalKernel | Any, journal: KernelJournal, operation_id: str) -> None:
    if not operation_id:
      raise ValueError("canonical kernel operation_id must not be empty")
    self.kernel = kernel
    self.journal = journal
    self.operation_id = operation_id

  async def transition(
    self,
    input: dict[str, Any],
    *,
    input_id: str | None = None,
    observed_at_ms: str | None = None,
  ) -> CanonicalTransition:
    envelope = json.dumps({
      "abi_version": KERNEL_ABI_VERSION,
      "operation_id": self.operation_id,
      "input_id": input_id or f"python-input-{uuid.uuid4()}",
      "observed_at_ms": observed_at_ms or str(int(time.time() * 1000)),
      "input": input,
    }, separators=(",", ":"))
    await self.journal.stage_outbound_envelope(self.operation_id, envelope)
    try:
      transition = await self._transition_envelope(envelope, cas_retries_left=1, checkpoint_retries_left=1)
      await self.journal.clear_outbound_envelope(self.operation_id)
      return transition
    except (CanonicalKernelRebuildRequiredError, CanonicalKernelRejectedError):
      await self.journal.clear_outbound_envelope(self.operation_id)
      raise

  async def restore(self) -> Any:
    checkpoint = await self.journal.latest_checkpoint(self.operation_id)
    records = await self.journal.records_after(
      self.operation_id, checkpoint.covered_head if checkpoint else None,
    )
    return self.kernel.restore(
      checkpoint.checkpoint_bytes if checkpoint else None,
      [entry.record_bytes for entry in records],
    )

  async def drain_outbound_envelope(self) -> CanonicalTransition | None:
    envelope = await self.journal.read_outbound_envelope(self.operation_id)
    if envelope is None:
      return None
    try:
      transition = await self._transition_envelope(envelope, cas_retries_left=1, checkpoint_retries_left=1)
      await self.journal.clear_outbound_envelope(self.operation_id)
      return transition
    except (CanonicalKernelRebuildRequiredError, CanonicalKernelRejectedError):
      await self.journal.clear_outbound_envelope(self.operation_id)
      raise

  async def checkpoint(self) -> InstalledCheckpoint:
    candidate: CanonicalCheckpoint = self.kernel.checkpoint_candidate()
    through = _chain_position(candidate.through_step_seq, "checkpoint through_step_seq")
    previous = await self.journal.latest_checkpoint(self.operation_id)
    try:
      installed = await self.journal.compare_and_install_checkpoint(
        self.operation_id,
        previous.checkpoint_id if previous else None,
        candidate.covered_head,
        CheckpointCandidate(
          checkpoint_id=candidate.ack_token,
          through_step_seq=through,
          state_digest=candidate.state_digest,
          checkpoint_bytes=candidate.checkpoint_bytes,
        ),
      )
    except JournalCasConflictError:
      winner = await self.journal.latest_checkpoint(self.operation_id)
      if winner is None or winner.checkpoint_id != candidate.ack_token:
        raise
      installed = winner
    await self.journal.ack_checkpoint(self.operation_id, installed.checkpoint_id)
    self.kernel.ack_checkpoint(candidate.through_step_seq, candidate.covered_head)
    await self.journal.prune_acked_prefix(self.operation_id)
    return InstalledCheckpoint(**{**installed.__dict__, "acknowledged": True})

  async def _transition_envelope(
    self, envelope: str, *, cas_retries_left: int, checkpoint_retries_left: int,
  ) -> CanonicalTransition:
    preparation = self.kernel.prepare(envelope)
    if isinstance(preparation, CanonicalRejected) or getattr(preparation, "status", None) == "rejected":
      code = getattr(preparation, "code", "kernel_rejected")
      message = getattr(preparation, "message", "canonical input rejected")
      if code == "checkpoint_required" and checkpoint_retries_left:
        await self.checkpoint()
        return await self._transition_envelope(
          envelope, cas_retries_left=cas_retries_left, checkpoint_retries_left=checkpoint_retries_left - 1,
        )
      raise CanonicalKernelRejectedError(code, message)
    if getattr(preparation, "status", None) == "replayed":
      return CanonicalTransition(
        input_json=envelope,
        step_seq=_chain_position(preparation.step_seq, "replayed step_seq"),
        record_digest=preparation.record_digest,
        planned_step=_planned_step(preparation.planned_step_json),
        checkpoint_advice=None,
        replayed=True,
      )

    step_seq = _chain_position(preparation.step_seq, "prepared step_seq")
    appended = False
    try:
      receipt = await self.journal.compare_and_append(
        self.operation_id,
        preparation.expected_head,
        JournalRecordInput(step_seq, preparation.record_digest, preparation.record_bytes),
      )
      appended = True
      committed = self.kernel.commit(preparation.prepare_token, receipt.record_digest)
      if committed.record_digest != preparation.record_digest or _chain_position(
        committed.step_seq, "committed step_seq"
      ) != step_seq:
        raise RuntimeError("canonical commit receipt disagrees with the durably appended record")
      advice = _advice(committed.checkpoint_advice_json)
      transition = CanonicalTransition(
        envelope, step_seq, committed.record_digest, _planned_step(committed.planned_step_json), advice, False,
      )
      if advice is not None:
        await self.checkpoint()
      return transition
    except Exception as error:
      if appended:
        try:
          await self.restore()
        except Exception as restore_error:
          raise CanonicalKernelRebuildRequiredError(
            "canonical record is durable, commit failed, and journal rebuild also failed",
          ) from restore_error
        raise CanonicalKernelRebuildRequiredError(
          "canonical record is durable but commit could not be published; runtime rebuilt from journal",
        ) from error
      try:
        self.kernel.abort(preparation.prepare_token)
      except Exception as abort_error:
        raise RuntimeError("canonical append failed and prepare could not be aborted") from abort_error
      if cas_retries_left and isinstance(error, JournalCasConflictError):
        await self.restore()
        return await self._transition_envelope(
          envelope, cas_retries_left=cas_retries_left - 1, checkpoint_retries_left=checkpoint_retries_left,
        )
      raise


def canonical_action_from_planned_step(planned_step: dict[str, Any]) -> KernelRunnerAction | None:
  disposition = _object(planned_step.get("disposition"))
  if disposition.get("kind") == "terminal":
    terminal = _object(disposition.get("terminal"))
    usage = _object(terminal.get("usage"))
    termination = str(terminal.get("kind") or "failed")
    turns = int(usage.get("turns") or 0)
    result = _object(terminal.get("result"))
    workflow_outcome = _object(terminal.get("outcome"))
    pace_decision = None
    if terminal.get("kind") == "agent":
      termination = str(result.get("termination") or "completed")
      turns = int(result.get("turns_used") or turns)
      pace = _object(result.get("pace_decision"))
      if pace:
        pace_decision = {
          "action": str(pace.get("action") or "stop"),
          **({"delay_ms": int(pace["delay_ms"])} if pace.get("delay_ms") is not None else {}),
          "reason": str(pace.get("reason") or ""),
          **({"coerced_from": str(pace["coerced_from"])} if pace.get("coerced_from") else {}),
        }
    elif terminal.get("kind") == "workflow":
      termination = str(_object(terminal.get("outcome")).get("status") or "completed")
    elif terminal.get("kind") == "cancelled":
      termination = str(terminal.get("reason") or "cancelled")
    elif terminal.get("kind") == "failed":
      termination = "context_overflow" if _object(terminal.get("failure")).get("code") == "provider_recovery_exhausted" else "error"
    return KernelRunnerAction(
      kind="done",
      result=SimpleNamespace(
        termination=termination,
        turns_used=turns,
        total_tokens_used=int(usage.get("input_tokens") or 0) + int(usage.get("output_tokens") or 0),
        workflow_outcome=workflow_outcome,
        pace_decision=pace_decision,
      ),
    )

  effects = disposition.get("effects") or []
  if not effects:
    return None
  if len(effects) != 1:
    raise RuntimeError(f"Python runner expects one canonical effect at a time, received {len(effects)}")
  envelope = _object(effects[0])
  effect_id = str(envelope.get("effect_id") or "")
  effect = _object(envelope.get("effect"))
  kind = effect.get("kind")
  if not effect_id:
    raise RuntimeError("canonical effect is missing effect_id")
  if kind == "call_provider":
    return KernelRunnerAction(
      kind="call_provider", effect_id=effect_id,
      context=_context_from_kernel(_object(effect.get("context"))),
      tools=[ToolSchema(str(t.get("name") or ""), str(t.get("description") or ""), json.dumps(t.get("parameters") or {}))
             for t in effect.get("tools") or [] if isinstance(t, dict)],
    )
  if kind == "execute_tools":
    return KernelRunnerAction(
      kind="execute_tool", effect_id=effect_id,
      calls=[ToolCall(str(c.get("call_id") or ""), str(c.get("name") or ""), json.dumps(c.get("arguments") or {}))
             for c in effect.get("calls") or [] if isinstance(c, dict)],
    )
  if kind == "request_approval":
    return KernelRunnerAction(kind="request_approval", effect_id=effect_id, requests=[
      {"call_id": str(r.get("call_id") or ""), "tool": str(r.get("tool_name") or ""),
       "arguments": json.dumps(r.get("arguments") or {}), "reason": str(r.get("reason") or "")}
      for r in effect.get("requests") or [] if isinstance(r, dict)
    ])
  if kind == "spawn_tasks":
    nodes = []
    for raw in effect.get("tasks") or []:
      task = _object(raw)
      spec = _object(task.get("spec"))
      metadata = _object(spec.get("metadata"))
      nodes.append({
        "agent_id": str(task.get("task_id") or ""),
        "task_id": str(task.get("task_id") or ""),
        "attempt_id": str(task.get("attempt_id") or ""),
        "launch_token": str(task.get("launch_token") or ""),
        "node_id": str(task.get("node_id") or ""),
        "goal": str(spec.get("goal") or ""),
        "role": str(spec.get("role") or "custom"),
        "isolation": str(spec.get("isolation") or "shared"),
        "context_inheritance": str(spec.get("context_inheritance") or "none"),
        **metadata,
      })
    return KernelRunnerAction(kind="spawn_workflow", effect_id=effect_id, nodes=nodes, budget=effect.get("budget"))
  if kind == "preempt_tasks":
    attempts = [
      {
        "task_id": str(_object(a).get("task_id") or ""),
        "attempt_id": str(_object(a).get("attempt_id") or f"{_object(a).get('task_id') or ''}:attempt:1"),
      }
      for a in (effect.get("attempts") or [])
    ]
    return KernelRunnerAction(kind="preempt_sub_agents", effect_id=effect_id,
                              agent_ids=[str(a["task_id"]) for a in attempts],
                              attempts=attempts,
                              reason=str(effect.get("reason") or ""))
  if kind == "persist_memory":
    return KernelRunnerAction(kind="persist_memory", effect_id=effect_id, memory=_object(effect.get("memory")))
  if kind == "query_memory":
    return KernelRunnerAction(kind="query_memory", effect_id=effect_id, query=_object(effect.get("query")),
                              requested_k=int(effect.get("requested_k") or 0))
  if kind == "archive_page_out":
    payload = _object(effect.get("payload"))
    archived: list[Message] = []
    try:
      archived = [_message_from_kernel(_object(v)) for v in json.loads(str(payload.get("content") or ""))]
    except (ValueError, TypeError):
      pass
    compressed = next(
      (obs for obs in (planned_step.get("observations") or []) if obs.get("kind") == "compressed"),
      None,
    )
    pressure_action = str(_object(compressed).get("action") or "") if compressed else ""
    tier = (
      "semantic" if pressure_action in ("context_collapse", "auto_compact") else "durable"
    ) if pressure_action else None
    return KernelRunnerAction(
      kind="archive_page_out", effect_id=effect_id, archived=archived,
      **({"action": pressure_action} if pressure_action else {}),
      **({"summary": str(compressed.get("summary"))} if compressed and compressed.get("summary") else {}),
      **({"tier": tier} if tier else {}),
    )
  if kind == "load_payload":
    return KernelRunnerAction(kind="load_payload", effect_id=effect_id,
                              handle_id=str(effect.get("handle_id") or ""),
                              payload_ref=str(effect.get("payload_ref") or ""))
  if kind == "evaluate_milestone":
    request = _object(effect.get("request"))
    return KernelRunnerAction(kind="evaluate_milestone", effect_id=effect_id,
                              phase_id=str(request.get("phase_id") or ""), criteria=[], required_evidence=[])
  return KernelRunnerAction(
    kind="unsupported_effect",
    effect_id=effect_id,
    effect_kind=str(kind),
  )


class CanonicalRunnerRuntime:
  """Canonical operation surface used by production runner callsites."""

  def __init__(
    self, kernel: CanonicalKernel, journal: KernelJournal, operation_id: str, *,
    max_context_tokens: int, max_turns: int | None = None, max_total_tokens: int | None = None,
    max_wall_ms: int | None = None, memory_binding_id: str = "python-memory",
    persist_payload: Any = None,
  ) -> None:
    self.host = CanonicalKernelHost(kernel, journal, operation_id)
    self._config: dict[str, Any] = {
      "execution_policy": {
        "max_context_tokens": max_context_tokens,
        **({"max_turns": max_turns} if max_turns is not None else {}),
        **({"max_total_tokens": str(max_total_tokens)} if max_total_tokens is not None else {}),
        **({"max_wall_ms": str(max_wall_ms)} if max_wall_ms is not None else {}),
      },
      "host_effect_support": {"supported": [
        "call_provider", "execute_tools", "request_approval", "spawn_tasks",
        "preempt_tasks", "persist_memory", "query_memory", "archive_page_out",
        "load_payload", "evaluate_milestone",
      ]},
      "kernel_limits": {
        "max_json_depth": 64,
        "max_collection_entries": 65_536,
        "collection_limits": {
          "tool_catalog": 4_096, "skill_catalog": 4_096,
          "knowledge_entries": 65_536, "initial_messages": 65_536,
          "capability_grants": 65_536, "governance_rules": 65_536,
        },
      },
    }
    self._initial_context: dict[str, list[dict[str, Any]]] = {"messages": [], "knowledge": [], "capabilities": []}
    self._memory_binding_id = memory_binding_id
    self._configured = False
    self._started = False
    self._turns = 0
    self._last_action: KernelRunnerAction | None = None
    self._observations: list[dict[str, Any]] = []
    self._new_messages: list[Message] = []
    self._spawned_tasks = 0
    self._payload_inline_threshold = 50 * 1024
    self._payload_preview_bytes = 2 * 1024
    self._persist_payload = persist_payload

  @property
  def operation_id(self) -> str:
    return self.host.operation_id

  @property
  def journal(self) -> KernelJournal:
    return self.host.journal

  def turn(self) -> int:
    return self._turns

  def is_terminal(self) -> bool:
    return self.host.kernel.lifecycle() in {"completed", "cancelled", "failed"}

  def recovery_content_bytes(self) -> int:
    return max(1024, int(self._config["execution_policy"]["max_context_tokens"]) * 4)

  def preserved_refs(self) -> list[str]:
    return []

  def local_subagents_spawned(self) -> int:
    return self._spawned_tasks

  def drain_host_observations(self) -> list[dict[str, Any]]:
    observations, self._observations = self._observations, []
    return observations

  def drain_new_messages(self) -> list[Message]:
    messages, self._new_messages = self._new_messages, []
    return messages

  async def restore(self) -> None:
    await self.host.restore()
    lifecycle = self.host.kernel.lifecycle()
    self._configured = lifecycle != "created"
    self._started = lifecycle not in {"created", "configured"}
    await self.host.drain_outbound_envelope()
    self._last_action = self._current_action()

  def resume_action(self) -> KernelRunnerAction | None:
    self._last_action = self._current_action()
    return self._last_action

  async def start_agent(
    self,
    task: dict[str, Any],
    run_spec: dict[str, Any] | None = None,
  ) -> KernelRunnerAction | None:
    await self._ensure_configured()
    goal = str(task.get("goal") or "")
    action = await self._commit({"kind": "start_operation", "entry": {
      "kind": "agent",
      "task": {"goal": goal, "criteria": task.get("criteria") or []},
      **({"run_spec": self._logical_run_spec(run_spec, goal)} if run_spec is not None else {}),
    }, "initial_context": self._initial_context})
    self._started = True
    return action

  async def start_workflow(self, spec: dict[str, Any]) -> KernelRunnerAction | None:
    await self._ensure_configured()
    action = await self._commit({"kind": "start_operation", "entry": {
      "kind": "workflow", "spec": self._workflow_spec(spec),
    }, "initial_context": self._initial_context})
    self._started = True
    return action

  async def apply_host_event(self, event: dict[str, Any]) -> KernelRunnerAction | None:
    if not self._started and self._apply_bootstrap(event):
      return None
    kind = event.get("kind")
    if kind == "unsupported_effect":
      return await self._commit(canonical_unsupported_effect_resolution(
        str(event.get("effect_id") or ""),
        str(event.get("effect_kind") or ""),
      ))
    if kind == "provider_result":
      message = _object(event.get("message"))
      self._turns += 1
      self._new_messages.append(_message_from_kernel(message))
      return await self._resolve(event, {"kind": "provider", "outcome": {
        "kind": "completed", "message": self._provider_message(message),
        **({"observed_input_tokens": int(event["observed_input_tokens"])}
           if event.get("observed_input_tokens") is not None else {}),
        **({"observed_output_tokens": int(event["observed_output_tokens"])}
           if event.get("observed_output_tokens") is not None else {}),
        **({"stop_reason": self._provider_stop_reason(event.get("stop_reason"))}
           if self._provider_stop_reason(event.get("stop_reason")) else {}),
      }})
    if kind == "provider_error":
      message = str(event.get("message") or "")
      if re.search(r"context|token.*limit|too long", message, re.I):
        return await self._resolve(event, {"kind": "provider", "outcome": {"kind": "context_overflow"}})
      return await self._failed(event, "transport_exhausted", message, True)
    if kind == "tool_results":
      results = []
      for raw in event.get("results") or []:
        result = _object(raw)
        output = str(result.get("output") or "")
        call_id = str(result.get("call_id") or "")
        self._new_messages.append(Message(role="tool", content=output))
        if len(output.encode()) > self._payload_inline_threshold and self._persist_payload is not None:
          persisted = await self._persist_payload(call_id, output, self._payload_preview_bytes)
          results.append({"kind": "external", "call_id": call_id,
                          "payload_ref": persisted["payload_ref"], "digest": persisted["digest"],
                          "original_size": str(persisted["original_size"]),
                          "preview": persisted["preview"],
                          **({"is_error": True} if result.get("is_error") else {}),
                          "disposition": "fatal" if result.get("is_fatal") else "recoverable"})
          continue
        results.append({"kind": "inline", "call_id": call_id, "result": {
          "output": output, **({"is_error": True} if result.get("is_error") else {}),
          "disposition": "fatal" if result.get("is_fatal") else "recoverable",
          **({"tokens": int(result["token_count"])} if result.get("token_count") is not None else {}),
        }})
      return await self._resolve(event, {"kind": "tools", "results": results})
    if kind == "approval_result":
      return await self._resolve(event, {"kind": "approval",
        "approved_call_ids": event.get("approved_calls") or [], "denied_call_ids": event.get("denied_calls") or []})
    if kind == "workflow_spawn_result":
      spawn = self._last_action if self._last_action and self._last_action.kind == "spawn_workflow" else None
      nodes = spawn.nodes if spawn and spawn.nodes else []
      self._spawned_tasks += len(nodes)
      return await self._resolve(event, {"kind": "tasks_spawned", "attempts": [
        {"task_id": str(_object(node).get("task_id") or _object(node).get("agent_id") or ""),
         "attempt_id": str(_object(node).get("attempt_id") or ""),
         "outcome": {"status": "started"}} for node in nodes
      ]})
    if kind == "preempt_result":
      preempt = self._last_action if self._last_action and self._last_action.kind == "preempt_sub_agents" else None
      attempts = getattr(preempt, "attempts", None) or [
        {"task_id": agent_id, "attempt_id": f"{agent_id}:attempt:1"}
        for agent_id in (preempt.agent_ids if preempt else [])
      ]
      return await self._resolve(event, {"kind": "tasks_preempted", "attempts": [
        {**_object(attempt), "outcome": {"status": "preempted"}} for attempt in attempts
      ]})
    if kind == "sub_agent_completed":
      raw, result = _object(event.get("result")), _object(_object(event.get("result")).get("result"))
      task_id = str(raw.get("agent_id") or "")
      pending = next((entry for entry in self._pending_effects()
                      if _object(entry.get("effect")).get("kind") == "spawn_tasks"
                      and any(str(_object(task).get("task_id") or "") == task_id
                              for task in _object(entry.get("effect")).get("tasks") or [])), {})
      launch = next((task for task in _object(pending.get("effect")).get("tasks") or []
                     if str(_object(task).get("task_id") or "") == task_id), {})
      final = _object(result.get("final_message"))
      submitted = [_object(node) for node in raw.get("submitted_nodes") or []]
      child: dict[str, Any] = {
        "kind": "child_completed", "task_id": task_id,
        "attempt_id": str(_object(launch).get("attempt_id") or f"{task_id}:attempt:1"),
        "result": {"status": "completed" if result.get("termination") == "completed" else "failed",
                   **({"output": str(final["content"])} if final.get("content") else {}),
                   **({"error": str(result.get("termination") or "failed")}
                      if result.get("termination") not in {"completed", "max_turns", "token_budget"} else {}),
                   "usage": {"input_tokens": "0", "output_tokens": str(result.get("total_tokens_used") or 0),
                             "turns": int(result.get("turns_used") or 0)}},
      }
      if submitted:
        child["parent_requests"] = [{"kind": "append_workflow_nodes",
                                      "nodes": self._workflow_spec({"nodes": submitted}).get("nodes") or []}]
      return await self._commit({"kind": "deliver_external_event", "event": child})
    if kind == "memory_persist_result":
      return await (self._failed(event, "storage_unavailable", str(event["error"]), True)
                    if event.get("error") else self._resolve(event, {"kind": "memory_persisted", "receipt": {
                      "binding_id": self._memory_binding_id,
                      "record_ref": str(event.get("record_ref") or f"memory:{uuid.uuid4()}"),
                      "digest": str(event.get("digest") or self._sha256(str(event.get("record_ref") or event.get("effect_id") or ""))),
                    }}))
    if kind == "memory_query_result":
      if event.get("error"):
        return await self._failed(event, "storage_unavailable", str(event["error"]), True)
      recalls = []
      for raw_hit in event.get("hits") or []:
        hit, record = _object(raw_hit), _object(_object(raw_hit).get("record"))
        recalls.append({"record_ref": str(record.get("record_id") or f"memory:{uuid.uuid4()}"),
                        "name": str(record.get("name") or ""), "kind": str(record.get("kind") or "reference"),
                        "content": str(record.get("content") or ""),
                        **({"score": hit["score"]} if isinstance(hit.get("score"), (float, int)) else {})})
      return await self._resolve(event, {"kind": "memory_queried", "recalls": recalls})
    if kind == "page_out_archive_result":
      if event.get("error"):
        return await self._failed(event, "storage_unavailable", str(event["error"]), True)
      pending = next((entry for entry in self._pending_effects()
                      if str(entry.get("effect_id") or "") == str(event.get("effect_id") or "")), {})
      effect, payload = _object(pending.get("effect")), _object(_object(pending.get("effect")).get("payload"))
      content = str(payload.get("content") or "")
      return await self._resolve(event, {"kind": "page_out_archived", "receipt": {
        "handle_id": str(effect.get("handle_id") or ""),
        "payload_ref": str(event.get("archive_ref") or f"payload:{uuid.uuid4()}"),
        "digest": str(payload.get("digest") or self._sha256(content)),
        "original_size": str(payload.get("original_size") or len(content.encode())),
      }})
    if kind == "milestone_result":
      result = _object(event.get("result"))
      return await self._resolve(event, {"kind": "milestone_evaluated", "result": {
        "phase_id": str(result.get("phase_id") or ""), "passed": bool(result.get("passed")),
        **({"notes": str(result["reason"])} if not result.get("passed") and result.get("reason") else {}),
      }})
    if kind == "payload_loaded":
      return await self._resolve(event, {"kind": "payload_loaded",
                                         "handle_id": str(event.get("handle_id") or ""),
                                         "payload": event.get("payload")})
    if kind == "payload_load_failed":
      return await self._failed(event, "storage_unavailable", str(event.get("error") or "payload load failed"), True)
    if kind == "deliver_signal":
      return await self._commit({"kind": "deliver_external_event", "event": self._signal(event)})
    if kind == "add_knowledge_message":
      return await self._commit({"kind": "host_control", "command": {"kind": "seed_knowledge", "entries": [{
        "content": str(event.get("content") or ""), **({"key": event["key"]} if event.get("key") else {}),
        **({"tokens": int(event["tokens"])} if event.get("tokens") is not None else {}),
        **({"pinned": True} if event.get("pinned") else {}),
      }]}})
    if kind == "remove_knowledge":
      return await self._commit({"kind": "host_control", "command": {
        "kind": "apply_knowledge_mutation", "mutation": {"remove": [str(event.get("key") or "")]}}})
    if kind == "skill_deactivated":
      return await self._commit({"kind": "host_control", "command": {
        "kind": "apply_skill_activation", "deactivate": [str(event.get("name") or "")]}})
    if kind == "capability_command":
      return await self._commit({"kind": "host_control", "command": self._capability_command(_object(event.get("command")))})
    if kind == "add_history_message":
      raise CanonicalKernelRejectedError("unsupported_host_event",
                                         "running ABI v3 operations accept history only through effects or external events")
    if kind == "cancel_operation":
      action = await self._commit({"kind": "host_control", "command": {
        "kind": "cancel", "reason": event.get("reason") or "user",
        "pending_call_ids": event.get("pending_call_ids") or [],
      }})
      # The canonical terminal state does not itself carry a presentation observation. Preserve the
      # durable host audit record expected by SessionLog consumers at the transition boundary.
      self._observations.append({
        "kind": "operation_cancelled",
        "operation_id": self.operation_id,
        "reason": event.get("reason") or "user",
        "pending_call_ids": event.get("pending_call_ids") or [],
      })
      return action
    if kind == "update_task":
      return await self._commit({"kind": "host_control", "command": {"kind": "update_task", "update": event.get("update")}})
    raise CanonicalKernelRejectedError("unsupported_host_event", f"canonical ABI has no input for {kind}")

  async def _ensure_configured(self) -> None:
    if not self._configured:
      await self._commit({"kind": "configure_operation", "config": self._config})
      self._configured = True

  async def _commit(self, input: dict[str, Any]) -> KernelRunnerAction | None:
    next_input = input
    while True:
      transition = await self.host.transition(next_input)
      if not transition.replayed:
        self._observations.extend(_object(item) for item in transition.planned_step.get("observations") or [])
        if transition.checkpoint_advice:
          self._observations.append({"kind": "checkpoint_advised", **transition.checkpoint_advice})
      self._last_action = canonical_action_from_planned_step(transition.planned_step)
      if self._last_action is None or self._last_action.kind != "unsupported_effect":
        return self._last_action
      next_input = canonical_unsupported_effect_resolution(
        self._last_action.effect_id,
        self._last_action.effect_kind or "",
      )

  async def _resolve(self, event: dict[str, Any], result: dict[str, Any]) -> KernelRunnerAction | None:
    return await self._commit({"kind": "resolve_effect", "effect_id": str(event.get("effect_id") or ""),
                               "outcome": {"status": "succeeded", "result": result}})

  async def _failed(self, event: dict[str, Any], kind: str, message: str, retryable: bool) -> KernelRunnerAction | None:
    return await self._commit({"kind": "resolve_effect", "effect_id": str(event.get("effect_id") or ""),
                               "outcome": {"status": "failed", "failure": {
                                 "kind": kind, "message": message, "retryable": retryable}}})

  def _current_action(self) -> KernelRunnerAction | None:
    terminal = self.host.kernel.terminal_json()
    if terminal:
      return canonical_action_from_planned_step({"disposition": {"kind": "terminal", "terminal": json.loads(terminal)}})
    return canonical_action_from_planned_step({"disposition": {
      "kind": "effects", "effects": json.loads(self.host.kernel.pending_effects_json())}})

  def _pending_effects(self) -> list[dict[str, Any]]:
    raw = json.loads(self.host.kernel.pending_effects_json())
    return [_object(value) for value in raw] if isinstance(raw, list) else []

  @staticmethod
  def _sha256(value: str) -> str:
    return f"sha256:{hashlib.sha256(value.encode()).hexdigest()}"

  @staticmethod
  def _provider_stop_reason(value: Any) -> str | None:
    if not isinstance(value, str) or not value:
      return None
    return value.lower() if value.lower() in {
      "end_turn", "tool_use", "max_tokens", "stop_sequence", "content_filter",
    } else "other"

  @staticmethod
  def _logical_run_spec(raw: dict[str, Any], goal: str) -> dict[str, Any]:
    capability_filter = _object(raw.get("capability_filter"))
    out: dict[str, Any] = {
      "goal": str(raw.get("goal") or goal),
      **({"role": raw["role"]} if raw.get("role") else {}),
      **({"isolation": raw["isolation"]} if raw.get("isolation") else {}),
      **({"context_inheritance": raw["context_inheritance"]} if raw.get("context_inheritance") else {}),
      **({"verification_contract_id": raw["verification_contract_id"]} if raw.get("verification_contract_id") else {}),
      **({"exposure_baseline": raw["exposure_baseline"]} if "exposure_baseline" in raw else {}),
      **({"metadata": raw["metadata"]} if isinstance(raw.get("metadata"), dict) else {}),
    }
    if capability_filter.get("allowed_kinds") or capability_filter.get("allowed_ids"):
      out["capability_filter"] = {
        **({"allowed_kinds": capability_filter["allowed_kinds"]} if capability_filter.get("allowed_kinds") else {}),
        **({"allowed_ids": capability_filter["allowed_ids"]} if capability_filter.get("allowed_ids") else {}),
      }
    if isinstance(raw.get("loop_round"), dict):
      loop = _object(raw["loop_round"])
      out["loop_round"] = {
        **({"max_rounds": int(loop["max_rounds"])} if loop.get("max_rounds") is not None else {}),
        **({"min_sleep_ms": str(loop["min_sleep_ms"])} if loop.get("min_sleep_ms") is not None else {}),
        **({"max_sleep_ms": str(loop["max_sleep_ms"])} if loop.get("max_sleep_ms") is not None else {}),
        **({"default_action": loop["default_action"]} if loop.get("default_action") is not None else {}),
      }
    return out

  def _workflow_spec(self, raw: dict[str, Any]) -> dict[str, Any]:
    nodes = raw.get("nodes") if isinstance(raw.get("nodes"), list) else []
    ids = [f"wf-node{index}" for index in range(len(nodes))]
    lowered = []
    for index, value in enumerate(nodes):
      node = _object(value)
      unsupported = []
      if any(node.get(key) is not None for key in ("kind", "reducer", "loop", "classify", "tournament")):
        unsupported.append("kind")
      if node.get("trust") is not None and node.get("trust") != "trusted":
        unsupported.append("trust")
      if node.get("dep_policy", node.get("depPolicy")) not in (None, "all_success"):
        unsupported.append("dep_policy")
      if node.get("token_budget", node.get("tokenBudget")) is not None:
        unsupported.append("token_budget")
      if node.get("max_turns", node.get("maxTurns")) is not None:
        unsupported.append("max_turns")
      if node.get("max_wall_ms", node.get("maxWallMs")) is not None:
        unsupported.append("max_wall_ms")
      inheritance = node.get("context_inheritance", node.get("contextInheritance"))
      if unsupported:
        raise CanonicalKernelRejectedError(
          "unsupported_effect",
          f"workflow node {index} uses fields absent from canonical WorkflowNode: {', '.join(unsupported)}",
        )
      task_value, task = node.get("task"), _object(node.get("task"))
      goal = task_value if isinstance(task_value, str) else str(task.get("goal") or node.get("goal") or "")
      dependencies = node.get("depends_on", node.get("dependsOn", []))
      depends_on = [
        ids[int(value)] if isinstance(value, int) and 0 <= value < len(ids) else str(value)
        for value in dependencies if isinstance(dependencies, list)
      ]
      run_spec = self._logical_run_spec({
        "goal": goal, **({"role": node["role"]} if node.get("role") else {}),
        **({"isolation": node["isolation"]} if node.get("isolation") else {}),
        **({"context_inheritance": inheritance} if inheritance else {}),
        **({"metadata": {
          **({"model_hint": node.get("model_hint", node.get("modelHint"))}
             if node.get("model_hint", node.get("modelHint")) is not None else {}),
          **({"output_schema": node.get("output_schema", node.get("outputSchema"))}
             if node.get("output_schema", node.get("outputSchema")) is not None else {}),
        }} if node.get("model_hint", node.get("modelHint")) is not None
          or node.get("output_schema", node.get("outputSchema")) is not None else {}),
      }, goal)
      lowered.append({
        "node_id": ids[index], "task": {
          "goal": goal, **({"criteria": task["criteria"]} if task.get("criteria") else {}),
          **({"lane": task["lane"]} if task.get("lane") else {}),
        }, **({"depends_on": depends_on} if depends_on else {}), "run_spec": run_spec,
      })
    return {"nodes": lowered}

  @staticmethod
  def _signal(event: dict[str, Any]) -> dict[str, Any]:
    signal = _object(event.get("signal"))
    delivery_id = str(event.get("delivery_id") or uuid.uuid4())
    payload = signal.get("summary") if delivery_id.startswith("injected-") and isinstance(signal.get("summary"), str) else signal.get("payload") or {}
    return {
      "kind": "deliver_signal", "delivery_id": delivery_id, "attempt": int(event.get("attempt") or 1),
      "signal": {
        "signal_id": str(signal.get("signal_id") or signal.get("id") or uuid.uuid4()),
        **({"source": signal["source"]} if signal.get("source") else {}),
        "target": {"kind": "task", "task_id": str(signal["recipient"])}
                  if signal.get("recipient") else {"kind": "operation"},
        **({"urgency": signal["urgency"]} if signal.get("urgency") else {}),
        "payload": payload,
        **({"source_timestamp_ms": str(signal["timestamp_ms"])} if signal.get("timestamp_ms") is not None else {}),
        **({"dedupe_key": signal["dedupe_key"]} if signal.get("dedupe_key") else {}),
      },
    }

  @staticmethod
  def _capability_command(command: dict[str, Any]) -> dict[str, Any]:
    capability = _object(command.get("capability"))
    if command.get("action") == "mount":
      return {"kind": "apply_capability_patch", "patch": {"mount": [{
        "kind": str(capability.get("kind") or "tool"), "id": str(capability.get("id") or ""),
        **({"description": capability["description"]} if capability.get("description") else {}),
      }]}}
    return {"kind": "apply_capability_patch", "patch": {"unmount": [{
      "kind": str(command.get("kind") or "tool"), "id": str(command.get("id") or ""),
    }]}}

  def _apply_bootstrap(self, event: dict[str, Any]) -> bool:
    kind = event.get("kind")
    if kind in {"set_tokenizer"}:
      return True
    if kind == "set_tools":
      self._config["tool_catalog"] = event.get("tools") or []
      return True
    if kind == "add_system_message":
      self._initial_context["messages"].append({"role": "system", "content": str(event.get("content") or ""),
                                                **({"tokens": event["tokens"]} if event.get("tokens") is not None else {})})
      return True
    if kind == "add_knowledge_message":
      if self._started:
        return False
      self._initial_context["knowledge"].append({"content": str(event.get("content") or ""),
                                                 **({"key": event["key"]} if event.get("key") else {}),
                                                 **({"tokens": event["tokens"]} if event.get("tokens") is not None else {})})
      return True
    if kind == "preload_history":
      self._initial_context["messages"].extend(
        self._initial_message(_object(message)) for message in event.get("messages") or []
      )
      return True
    if kind == "set_available_skills":
      self._config["skill_catalog"] = event.get("skills") or []
      return True
    if kind == "load_governance_policy":
      governance = dict(_object(event.get("policy")))
      rate_limits = governance.get("rate_limits")
      if isinstance(rate_limits, list):
        governance["rate_limits"] = [
          {**_object(rule), "window_ms": str(_object(rule).get("window_ms") or 0)}
          for rule in rate_limits
        ]
      self._config["governance_policy"] = governance
      return True
    if kind == "set_plan_tool_enabled":
      self._feature_policy()["plan_tool_enabled"] = bool(event.get("enabled"))
      return True
    if kind == "set_stable_core_tools":
      self._feature_policy()["stable_core_tool_ids"] = event.get("tool_ids") or []
      return True
    if kind == "set_memory_enabled":
      self._feature_policy()["memory_enabled"] = bool(event.get("enabled"))
      if event.get("enabled"):
        self._config["memory_access"] = {"binding_id": self._memory_binding_id, "capabilities": {"read": True, "write": True}}
      return True
    if kind == "set_knowledge_enabled":
      self._feature_policy()["knowledge_enabled"] = bool(event.get("enabled"))
      return True
    if kind == "set_resource_quota":
      quota = dict(_object(event.get("quota")))
      window = quota.get("memory_writes_per_window")
      if isinstance(window, list):
        quota["memory_writes_per_window"] = {
          "max_events": int(window[0] if window else 0),
          "window_ms": str(window[1] if len(window) > 1 else 0),
        }
      self._config["resource_quota"] = quota
      return True
    if kind == "set_repeat_fuse":
      self._execution_policy()["repeat_fuse"] = {key: value for key, value in event.items() if key != "kind"}
      return True
    if kind == "set_criteria_gate":
      self._execution_policy()["criteria_gate_enabled"] = bool(event.get("enabled"))
      return True
    if kind == "set_knowledge_budget":
      self._context_policy()["knowledge_budget_ppm"] = round(float(event.get("ratio") or 0) * 1_000_000)
      return True
    if kind == "set_entropy_watch":
      watch = {key: value for key, value in event.items() if key != "kind"}
      if isinstance(watch.get("threshold"), float):
        watch["threshold_ppm"] = round(watch.pop("threshold") * 1_000_000)
      if isinstance(watch.get("hysteresis"), float):
        watch["hysteresis_ppm"] = round(watch.pop("hysteresis") * 1_000_000)
      self._execution_policy()["entropy_watch"] = watch
      return True
    if kind == "set_memory_policy":
      policy: dict[str, Any] = {}
      if event.get("stale_warning_days") is not None:
        policy["stale_warning_days"] = event["stale_warning_days"]
      if event.get("retrieval_top_k") is not None:
        policy["retrieval_top_k"] = event["retrieval_top_k"]
      if event.get("validation_enabled") is not None:
        policy["validation_enabled"] = event["validation_enabled"]
      if event.get("max_content_bytes") is not None:
        policy["max_content_bytes"] = event["max_content_bytes"]
      if event.get("max_name_length") is not None:
        policy["max_name_length"] = event["max_name_length"]
      if event.get("promotion_recall_threshold") is not None:
        policy["promotion_recall_threshold"] = str(event["promotion_recall_threshold"])
      self._config["memory_policy"] = policy
      return True
    if kind == "load_milestone_contract":
      contract = _object(event.get("contract"))
      self._config["verification_contracts"] = [{"contract_id": "python-default", "phases": [
        {"phase_id": str(_object(phase).get("id") or ""),
         "unlocks": [str(_object(item).get("id") or item) for item in _object(phase).get("unlocks") or []]}
        for phase in contract.get("phases") or []
      ]}]
      return True
    if kind == "add_history_message":
      if self._started:
        return False
      self._initial_context["messages"].append(self._initial_message(_object(event.get("message"))))
      return True
    if kind == "configure_run":
      self._merge_host_config(_object(event.get("config")))
      return True
    return False

  def _feature_policy(self) -> dict[str, Any]:
    value = _object(self._config.get("feature_policy"))
    self._config["feature_policy"] = value
    return value

  def _execution_policy(self) -> dict[str, Any]:
    return _object(self._config.get("execution_policy"))

  def _context_policy(self) -> dict[str, Any]:
    value = _object(self._config.get("context_policy"))
    self._config["context_policy"] = value
    return value

  @staticmethod
  def _initial_message(raw: dict[str, Any]) -> dict[str, Any]:
    if isinstance(raw.get("content"), list):
      return {
        "role": str(raw.get("role") or "user"),
        "content": encode_canonical_content_parts(raw["content"]),
        **({"tokens": int(raw["token_count"])} if raw.get("token_count") is not None else {}),
      }
    message = CanonicalRunnerRuntime._provider_message(raw)
    message.pop("tool_calls", None)
    return message

  def _merge_host_config(self, config: dict[str, Any]) -> None:
    if config.get("governance") is not None:
      governance = dict(_object(config["governance"]))
      rate_limits = governance.get("rate_limits")
      if isinstance(rate_limits, list):
        governance["rate_limits"] = [
          {**_object(rule), "window_ms": str(_object(rule).get("window_ms") or 0)}
          for rule in rate_limits
        ]
      self._config["governance_policy"] = governance
    if config.get("context_policy") is not None:
      self._config["context_policy"] = config["context_policy"]
    if config.get("signal_policy") is not None:
      signal = dict(_object(config["signal_policy"]))
      signal.pop("version", None)
      if signal.get("ttl_ms") is not None:
        signal["ttl_ms"] = str(signal["ttl_ms"])
      self._config["signal_policy"] = signal
    if config.get("scheduler_policy") is not None:
      scheduler = dict(_object(config["scheduler_policy"]))
      scheduler.pop("version", None)
      self._config["scheduler_policy"] = scheduler
    if config.get("resource_quota") is not None:
      quota = dict(_object(config["resource_quota"]))
      window = quota.get("memory_writes_per_window")
      if isinstance(window, list):
        quota["memory_writes_per_window"] = {
          "max_events": int(window[0] if window else 0),
          "window_ms": str(window[1] if len(window) > 1 else 0),
        }
      self._config["resource_quota"] = quota
    if config.get("budget_grant") is not None:
      grant = dict(_object(config["budget_grant"]))
      if grant.get("tokens") is not None:
        grant["tokens"] = str(grant["tokens"])
      self._config["budget_grant"] = grant
    if config.get("prompt_budget") is not None:
      self._context_policy()["prompt_budget"] = config["prompt_budget"]
    execution = self._execution_policy()
    if config.get("repeat_fuse") is not None:
      execution["repeat_fuse"] = config["repeat_fuse"]
    if config.get("criteria_gate") is not None:
      execution["criteria_gate_enabled"] = config["criteria_gate"]
    if config.get("knowledge_budget_ratio") is not None:
      self._context_policy()["knowledge_budget_ppm"] = round(float(config["knowledge_budget_ratio"]) * 1_000_000)
    if config.get("entropy_watch") is not None:
      entropy = dict(_object(config["entropy_watch"]))
      if isinstance(entropy.get("threshold"), (int, float)) and not isinstance(entropy.get("threshold"), bool):
        entropy["threshold_ppm"] = round(float(entropy.pop("threshold")) * 1_000_000)
      if isinstance(entropy.get("hysteresis"), (int, float)) and not isinstance(entropy.get("hysteresis"), bool):
        entropy["hysteresis_ppm"] = round(float(entropy.pop("hysteresis")) * 1_000_000)
      execution["entropy_watch"] = entropy
    if config.get("tool_dispatch_gate") is not None:
      self._feature_policy()["tool_dispatch_gate"] = config["tool_dispatch_gate"]
    reliability = _object(config.get("reliability"))
    if reliability:
      recovery: dict[str, Any] = {}
      if reliability.get("provider_recovery_attempts") is not None:
        recovery["provider_recovery_attempts"] = reliability["provider_recovery_attempts"]
      if reliability.get("output_recovery_attempts") is not None:
        recovery["output_recovery_attempts"] = reliability["output_recovery_attempts"]
      if recovery:
        self._config["recovery_policy"] = recovery
      if reliability.get("max_input_bytes") is not None:
        limits = dict(_object(self._config.get("kernel_limits")))
        limits["max_input_bytes"] = reliability["max_input_bytes"]
        self._config["kernel_limits"] = limits

  @staticmethod
  def _provider_message(raw: dict[str, Any]) -> dict[str, Any]:
    return {
      "role": str(raw.get("role") or "assistant"),
      "content": raw.get("content") if isinstance(raw.get("content"), str) else json.dumps(raw.get("content") or ""),
      **({"tool_calls": [{"call_id": str(_object(c).get("call_id") or _object(c).get("id") or ""),
                           "name": str(_object(c).get("name") or ""),
                           "arguments": _object(_object(c).get("arguments"))}
                          for c in raw.get("tool_calls") or []]} if raw.get("tool_calls") else {}),
    }


async def apply_host_event(
  runtime: CanonicalRunnerRuntime, pending: list[dict[str, Any]], event: dict[str, Any],
) -> list[dict[str, Any]]:
  await runtime.apply_host_event(event)
  observations = runtime.drain_host_observations()
  pending.extend(observations)
  return observations


async def maybe_host_action(
  runtime: CanonicalRunnerRuntime, pending: list[dict[str, Any]], event: dict[str, Any],
) -> KernelRunnerAction | None:
  action = await runtime.apply_host_event(event)
  pending.extend(runtime.drain_host_observations())
  return action


async def host_action(
  runtime: CanonicalRunnerRuntime, pending: list[dict[str, Any]], event: dict[str, Any],
) -> KernelRunnerAction:
  action = await maybe_host_action(runtime, pending, event)
  if action is None:
    raise RuntimeError("canonical kernel transition must return one host action")
  return action


async def start_agent(
  runtime: CanonicalRunnerRuntime,
  pending: list[dict[str, Any]],
  task: dict[str, Any],
  run_spec: dict[str, Any] | None = None,
) -> KernelRunnerAction:
  action = await runtime.start_agent(task, run_spec)
  pending.extend(runtime.drain_host_observations())
  if action is None:
    raise RuntimeError("canonical agent root must return one host action")
  return action


async def start_workflow(
  runtime: CanonicalRunnerRuntime,
  pending: list[dict[str, Any]],
  spec: dict[str, Any],
) -> KernelRunnerAction | None:
  action = await runtime.start_workflow(spec)
  pending.extend(runtime.drain_host_observations())
  return action
