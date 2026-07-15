from __future__ import annotations

import asyncio
import inspect
import json
import re
import time
import uuid
from collections.abc import AsyncIterator
from dataclasses import asdict, dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any, Awaitable, Callable, Literal

from deepstrike._kernel import (
  ContentPartObj,
  KernelRuntime,
  LoopPolicy,
  Message,
  SignalRouter,
  SkillMetadata,
  ToolCall,
  ToolResult,
  TaskUpdate,
)
from deepstrike.providers.base import LLMProvider, RenderedContext
from deepstrike.providers.stream import (
  DoneEvent,
  EntropyAlertEvent,
  EntropySample,
  EntropySampleEvent,
  ErrorEvent,
  StreamEvent,
  TextDelta,
  ToolCallEvent,
  ToolDeniedEvent,
  ToolResultEvent,
  ToolSuspendEvent,
  ToolArgumentRepairedEvent,
  PermissionRequestEvent,
  PermissionResolvedEvent,
  PermissionResponse,
  WorkflowNodesSubmittedEvent,
)
from deepstrike.runtime.execution_plane import ExecutionPlane, LocalExecutionPlane, RunContext
from deepstrike.tools.errors import format_tool_error
from deepstrike.governance import governance_policy_to_kernel_event
from deepstrike.runtime.kernel_event_log import kernel_observation_to_session_event
from deepstrike.runtime.context_policy import context_policy_v1, normalize_context_policy_v1
from deepstrike.runtime.kernel_step import (
  capability_marker,
  capability_skill,
  capability_tool,
  kernel_action,
  kernel_apply,
  kernel_maybe_action,
  message_to_kernel,
  skill_metadata_to_kernel,
  task_update_to_kernel,
  tool_result_to_kernel,
  tool_schema_to_kernel,
)
from deepstrike.runtime.replay_sanitize import sanitize_replay_text
from deepstrike.runtime.session_repair import (
  RecoveredNodeOutcome,
  build_llm_completed_event,
  build_run_terminal_event,
  repair_events_for_recovery,
)
from deepstrike.runtime.session_log import SessionEntry, SessionEvent, SessionLog
from deepstrike.runtime.archive import ArchiveStore
from deepstrike.runtime.os_profile import assert_native_profile
from deepstrike.runtime.run_group import GroupBudgetScope, GroupMember, RunGroup
from deepstrike.runtime.reliability import (
  BackgroundTaskErrorHandler,
  ManagedTaskScope,
  OperationContext,
)
from deepstrike.signals.types import RuntimeSignal, SignalDeliveryReceipt
from deepstrike.types.agent import WorkflowOutcome, workflow_node_outcome_from_kernel

if TYPE_CHECKING:
  from deepstrike.governance import Governance, GovernancePolicy
  from deepstrike.runtime.os_profile import OsProfile, SignalPolicy
  from deepstrike.knowledge.source import KnowledgeSource
  from deepstrike.memory.protocols import DreamStore, MemoryQuery, MemoryRecall, MemoryRecord, MemoryScope
  from deepstrike.signals.types import SignalSource
  from deepstrike.types.agent import AgentRunSpec, SubAgentResult, MilestonePolicy, MilestoneContract, MilestoneCheckResult
  from deepstrike.runtime.large_result_spool import LargeResultSpool


@dataclass
class SubAgentHarnessConfig:
  """When set on RuntimeOptions, spawned sub-agents run through AttemptLoop."""
  eval_provider: LLMProvider
  max_attempts: int = 3


@dataclass
class _InboundSignalDelivery:
  signal_id: str
  delivery_id: str
  delivery_attempt: int
  signal: RuntimeSignal
  ack: Callable[[], Awaitable[bool]]
  nack: Callable[[], Awaitable[bool]]


@dataclass
class MemoryWriteRateLimit:
  """Rolling-window memory-write rate limit for ResourceQuota."""
  max_writes: int
  window_ms: int


@dataclass
class ResourceQuota:
  """M2 resource quotas installed through the kernel JSON event ABI."""
  max_concurrent_subagents: int | None = None  # instantaneous; vehicle-scoped
  # L1 (RunGroup): max sub-agents spawned cumulatively across the governance domain; with a
  # run_group this spans N stateless top-level runs (seeded/charged via the group ledger).
  max_total_subagents: int | None = None
  max_spawn_depth: int | None = None
  memory_writes_per_window: MemoryWriteRateLimit | tuple[int, int] | None = None


@dataclass
class MemoryPolicy:
  """Long-term memory policy installed through the kernel JSON event ABI (`set_memory_policy`).

  Opt-in and kernel-enforced: ``validation_enabled=False`` admits writes without validation,
  ``max_content_bytes`` / ``max_name_length`` override the validation limits, and
  ``retrieval_top_k`` caps ``query_memory``'s emitted ``requested_k``. ``memory_path`` /
  ``stale_warning_days`` are carried for SDK recall I/O. Omitted fields keep the kernel defaults.
  """
  memory_path: str | None = None
  stale_warning_days: int | None = None
  retrieval_top_k: int | None = None
  validation_enabled: bool | None = None
  max_content_bytes: int | None = None
  max_name_length: int | None = None


@dataclass
class SchedulerPolicy:
  """Versioned deterministic DAG scheduler policy installed through ConfigureRun."""
  version: int
  critical_path_weight: int
  fanout_weight: int
  age_weight: int
  token_cost_weight: int


@dataclass
class PromptBudget:
  """Host-counted provider envelope and response reserves journaled before start."""
  prompt_overhead_tokens: int
  output_reserve_tokens: int
  safety_margin_tokens: int


@dataclass
class KernelReliability:
  """Bounded ABI-v2 reliability policy; omitted fields retain kernel defaults."""
  event_replay_capacity: int | None = None
  completed_effect_replay_capacity: int | None = None
  provider_recovery_attempts: int | None = None
  output_recovery_attempts: int | None = None
  host_effect_retry_attempts: int | None = None
  spool_threshold_bytes: int | None = None
  spool_preview_bytes: int | None = None
  # Max accepted ABI transactions retained for a portable KernelSnapshot rebuild.
  snapshot_input_limit: int | None = None
  # Max canonical JSON bytes accepted for one kernel input, 256..64MiB.
  max_input_bytes: int | None = None
  # Max canonical JSON bytes retained by the snapshot journal, 256..1GiB.
  snapshot_journal_bytes_limit: int | None = None


@dataclass
class TurnMetrics:
  """P0-C tool-gating telemetry: per-LLM-turn metrics emitted via ``RuntimeOptions.on_turn_metrics``.

  Pure observation — no behavior change. ``tools_exposed`` vs ``tools_called`` quantifies
  over-exposure; consecutive equal ``active_skill`` values measure skill dwell ``D``; the cache split
  gives the prompt-cache hit baseline. Mirrors the node SDK ``TurnMetrics``.
  """
  turn: int
  tools_exposed: int
  tools_called: int
  input_tokens: int
  cache_read_tokens: int
  cache_creation_tokens: int
  active_skill: str | None = None
  # I1: pro-rata per-slot attribution of cache_read_tokens (Anthropic only). Mirrors Node.
  cache_read_tokens_by_slot: "dict | None" = None


@dataclass
class RuntimeOptions:
  provider: LLMProvider
  session_log: SessionLog
  execution_plane: ExecutionPlane | None = None
  # Failures from run-owned best-effort tasks after their semantic owner has committed.
  on_background_task_error: BackgroundTaskErrorHandler | None = None
  # M1/G3 intelligence routing: resolve a per-node provider from a workflow node's ``model_hint``.
  # Returns None ⇒ fall back to ``provider``. Without this hook the hint is a no-op.
  provider_for: Callable[[str], LLMProvider | None] | None = None
  # M3/G4: when set, an ``isolation: "worktree"`` sub-agent runs inside a git worktree this manager
  # creates (and removes on completion), injected as ``RunContext.cwd``. None ⇒ no isolation.
  worktree_manager: Any = None
  compression_store: ArchiveStore | None = None
  max_tokens: int = 32_000
  max_turns: int = 25
  # M4/G5: cumulative token cap for this run (the kernel's ``max_total_tokens``); a node's
  # ``token_budget`` flows here for its child run. None ⇒ the kernel default.
  max_total_tokens: int | None = None
  timeout_ms: int | None = None
  agent_id: str | None = None
  memory_scope: "MemoryScope | None" = None
  # I4: run-start memory pre-fetch hook. Callable receiving kwarg `goal=`; returns a list of query
  # strings (or None). Each query becomes a dreamStore search; hits page into the knowledge
  # partition before turn 1. Requires dream_store + agent_id. Errs-open. Mirrors Node ``preQueryMemory``.
  pre_query_memory: "Callable[..., Any] | None" = None
  system_prompt: str | None = None
  initial_memory: list[str] | None = None
  skill_dir: str | Path | None = None
  dream_store: "DreamStore | None" = None
  #: M4: advisory callback when a recalled record crosses the promotion threshold. Keyword args:
  #: record_id: str, recall_count: int. The host/model decides whether to pin or promote to knowledge.
  on_promotion_suggested: "Callable[..., None] | None" = None
  knowledge_source: "KnowledgeSource | None" = None
  signal_source: "SignalSource | None" = None
  extensions: dict | None = None
  governance: "Governance | None" = None
  governance_policy: "GovernancePolicy | None" = None
  signal_policy: "SignalPolicy | dict | None" = None
  prompt_budget: PromptBudget | dict[str, Any] | None = None
  # Stable replayable context behavior. Public ratios are normalized to integer ppm on the wire.
  context_policy: dict[str, Any] | None = None
  scheduler_policy: SchedulerPolicy | dict[str, Any] | None = None
  kernel_reliability: KernelReliability | dict[str, Any] | None = None
  # Attempts allowed for a workflow node to satisfy its output schema, 1..16.
  workflow_schema_validation_attempts: int = 2
  resource_quota: ResourceQuota | dict[str, Any] | None = None
  # O6: the in-kernel repeat fuse — hard rungs above the soft no-progress STOP. When the model
  # re-issues the IDENTICAL tool call (same name AND args) deny_after turns in a row the kernel
  # denies it with a directive note; at terminate_after the run ends "no_progress". Same-tool/
  # different-args loops never trip it. None ⇒ kernel defaults (enabled, 5/8); a dict overrides
  # {"deny_after": int, "terminate_after": int}; False disables (legit fixed-argument polling).
  repeat_fuse: dict[str, int] | bool | None = None
  # O4: the turn-end criteria gate (the Stop-hook analog). When the model tries to finish while the
  # run's criteria stand, the kernel injects ONE self-check turn before accepting completion. Fires
  # at most once per run; runs without criteria are untouched. None ⇒ kernel default (enabled);
  # False accepts the first finish unconditionally.
  criteria_gate: bool | None = None
  # K2: max share of max_tokens the durable knowledge partition may occupy. Exceeding it emits a
  # knowledge_budget_exceeded observation (once per cache generation) and evicts the OLDEST
  # unpinned, non-skill entries at the next compaction/renewal boundary. Pinned entries and skill
  # pins are never budget-evicted. 0 disables. None ⇒ kernel default (0.25).
  knowledge_budget_ratio: float | None = None
  # Opt-in kernel entropy watch: threshold alerting over the per-turn session-entropy score
  # (entropy_sample events stream unconditionally regardless). A dict of
  # {"threshold": float, "hysteresis": float, "cooldown_turns": int, "notify_model": bool,
  #  "enabled": bool} — absent keys keep kernel defaults (0.65 / 0.1 / 4 / False); passing a dict
  # enables unless {"enabled": False}. notify_model additionally feeds the model a durable
  # [SIGNAL] directive when the alert fires. None ⇒ disabled (kernel default).
  entropy_watch: dict[str, Any] | None = None
  # K3: default lease (in turns) for every skill activation. After that many turns the kernel
  # auto-deactivates the skill — toolset re-widens, knowledge pin boundary-swept — exactly like an
  # explicit deactivate_skill(). None ⇒ activations are permanent (default). A repeat skill(name)
  # call refreshes the lease.
  skill_lease_turns: int | None = None
  # L1 (RunGroup): bind this runner to a governance domain shared by N peer sessions of one logical
  # run. The store must atomically reserve, settle, and release capacity. None ⇒ N=1.
  run_group: "RunGroup | None" = None
  memory_policy: MemoryPolicy | dict[str, Any] | None = None
  os_profile: "OsProfile | None" = None
  tokenizer: str | None = None
  enable_plan_tool: bool | None = None
  on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None
  # O5 (PreToolUse-hook analog): called for each kernel-APPROVED tool call just before it executes,
  # with {"call_id", "name", "arguments"}. Return {"block": True, "reason": ...} to veto — the call
  # never runs and the reason is fed back to the model as a denied tool result. The seam for
  # STATEFUL host policy (count repeats, per-resource budgets); keep static allow/deny in
  # governance_policy. A raising decision hook fails closed by default. Sync or async.
  on_tool_call: Callable[[dict], Awaitable[dict | None] | dict | None] | None = None
  on_tool_call_failure: str = "closed"
  # O5 (PostToolUse-hook analog): called for each executed result with {"call_id", "name",
  # "arguments", "output", "is_error"}. Return {"replace_output": str} to swap what the model sees
  # and/or {"note": str} to push a contextual note into the signal stream (same channel as
  # inject_note). Errs-open. Sync or async.
  on_tool_result: Callable[[dict], Awaitable[dict | None] | dict | None] | None = None
  on_permission_request: Callable[[PermissionRequestEvent], Awaitable[PermissionResponse | bool | dict[str, Any]] | PermissionResponse | bool | dict[str, Any]] | None = None
  sub_agent_orchestrator: Any | None = None
  # M5 v2.1: marks this runner as a workflow node (child of the workflow driver). A workflow node's
  # ``start_workflow`` FLATTENS to the parent kernel; a top-level run (unset) AUTO-PIVOTS — bootstraps +
  # drives the authored workflow in its own kernel, then resumes the reason loop with the outcome.
  is_workflow_node: bool = False
  sub_agent_harness: SubAgentHarnessConfig | None = None
  # G2: custom reducers for NodeKind::Reduce nodes, merged over the built-ins. A reduce node runs no LLM.
  reducers: dict | None = None
  dream_system_prompt: str | None = None
  milestone_policy: "MilestonePolicy | None" = None
  on_milestone_evaluate: Callable[[dict[str, Any]], Awaitable[Any] | Any] | None = None
  milestone_contract: "MilestoneContract | None" = None
  run_spec: "AgentRunSpec | None" = None
  # P0-A tool gating: a static per-run tool profile — only these tool ids (plus the meta-tools)
  # are exposed to the model each turn. Lowers to the same ``capability_filter`` sub-agents use;
  # byte-stable across the run, so it never busts the prompt-cache prefix. Augments ``run_spec``'s
  # filter when both set; synthesizes a minimal spec otherwise. None/empty => no gating.
  allowed_tool_ids: "list[str] | None" = None
  # P0-C: optional per-turn metrics sink for tool-gating telemetry (see ``TurnMetrics``). Pure
  # observation; invoked once per LLM turn. Never raises into the run loop (errors are swallowed).
  on_turn_metrics: "Callable[[TurnMetrics], None] | None" = None
  # P1-B/D stable-core: tool ids always exposed under skill gating. Empty/None ⇒ skills narrow to
  # exactly their declared tools + meta-tools. Opt-in: with no skill declaring tools, never engages.
  stable_core_tool_ids: "list[str] | None" = None
  result_spool: "LargeResultSpool | None" = None
  dream_provider: LLMProvider | None = None
  dream_summarizer: Callable[[list[Any], dict[str, Any]], Awaitable[str] | str] | None = None


OperationCancellationReason = Literal["user", "deadline", "lease_lost", "host_shutdown"]


def _pending_call_ids(action: Any) -> list[str]:
  kind = getattr(action, "kind", None)
  if kind == "call_provider":
    return [action.effect_id]
  if kind == "execute_tool":
    return [call.id for call in (action.calls or [])]
  if kind == "request_approval":
    return [request.call_id for request in (action.requests or [])]
  if kind == "spawn_workflow":
    return [str(node.agent_id) for node in (action.nodes or []) if getattr(node, "agent_id", None)]
  if kind == "preempt_sub_agents":
    return list(action.agent_ids or [])
  effect_id = getattr(action, "effect_id", None)
  return [effect_id] if effect_id else []


class RuntimeRunner:
  def __init__(self, opts: RuntimeOptions) -> None:
    if (
      isinstance(opts.workflow_schema_validation_attempts, bool)
      or not isinstance(opts.workflow_schema_validation_attempts, int)
      or not 1 <= opts.workflow_schema_validation_attempts <= 16
    ):
      raise ValueError("workflow_schema_validation_attempts must be an integer between 1 and 16")
    self._opts = opts
    self._interrupted = False
    self._cancellation_reason: OperationCancellationReason | None = None
    self._plane = opts.execution_plane or LocalExecutionPlane()
    self._active_kernel: KernelRuntime | None = None
    self._active_operation: OperationContext | None = None
    self._active_task_scope: ManagedTaskScope | None = None
    self._active_group_budget_scope: GroupBudgetScope | None = None
    self._pending_observations: list[dict] = []
    # K4: the active run's goal, kept for the renewal-boundary memory re-query.
    self._current_goal = ""
    self._current_session_id: str | None = None
    # O2 (system-reminder channel): host-pushed notes awaiting the next turn-boundary drain.
    self._injected_signals: list[RuntimeSignal] = []
    # Most recent kernel entropy sample of the active/last run (see `latest_entropy`).
    self._last_entropy_sample: EntropySample | None = None
    # Skill names whose content has already been pushed into the durable `knowledge` slot this
    # run — guards against re-pushing a duplicate entry if the model calls skill(name) again for
    # an already-active skill (loading is idempotent; the knowledge push should be too).
    self._knowledge_pushed_skills: set[str] = set()
    self._next_archive_start: int = 0
    self._pending_page_out_archives: list[tuple[int, int]] = []
    self._active_page_out_archive: tuple[int, int] | None = None
    # M5 v2.1: sub-workflow specs a top-level agent authored via ``start_workflow``, awaiting auto-drive
    # at the next safe point (after the tool turn resolves, kernel back in Reason — not suspended).
    self._pending_authored_workflows: list[Any] = []
    # The workflow driver consumes internal spawn effects and hands the committed provider
    # continuation back to the outer run loop through this transaction boundary.
    self._workflow_continuation_action: KernelRunnerAction | None = None

  @property
  def host_options(self) -> RuntimeOptions:
    """Host configuration (for coordinator / sub-agent spawn)."""
    return self._opts

  async def _close_active_scopes(self) -> None:
    group_scope = self._active_group_budget_scope
    self._active_group_budget_scope = None
    if group_scope is not None:
      await group_scope.release()

    task_scope = self._active_task_scope
    self._active_task_scope = None
    if task_scope is not None and task_scope.pending > 0:
      await task_scope.cancel()

    if self._active_operation is not None and self._active_operation.cancelled is not None:
      self._active_operation.cancelled.set()
    self._active_operation = None

  async def write_memory(
    self,
    memory: "MemoryRecord",
    *,
    session_id: str | None = None,
    agent_id: str | None = None,
  ) -> None:
    resolved_session_id = session_id or self._current_session_id
    resolved_agent_id = agent_id or self._opts.agent_id
    if not self._opts.dream_store or not resolved_agent_id:
      return

    observations: list[dict[str, Any]] = []
    target_runtime = self._create_syscall_runtime()
    action = kernel_maybe_action(target_runtime, observations, {"kind": "write_memory", "memory": asdict(memory)})
    if action is None:
      await self._append_memory_syscall_observations(resolved_session_id, observations)
      return
    if action.kind != "persist_memory":
      raise RuntimeError(f"write_memory returned unexpected kernel effect: {action.kind}")

    error: Exception | None = None
    try:
      canonical = _memory_record_from_mapping(action.memory or asdict(memory))
      await self._opts.dream_store.upsert(resolved_agent_id, canonical)
    except Exception as exc:
      error = exc
    kernel_apply(target_runtime, observations, {
      "kind": "memory_persist_result",
      "effect_id": action.effect_id,
      **({"error": format_tool_error(error)} if error else {}),
    })
    await self._append_memory_syscall_observations(resolved_session_id, observations)
    if error:
      raise error

  async def query_memory(
    self,
    query: "MemoryQuery",
    *,
    session_id: str | None = None,
    agent_id: str | None = None,
  ) -> list["MemoryRecall"]:

    resolved_session_id = session_id or self._current_session_id
    resolved_agent_id = agent_id or self._opts.agent_id
    if not self._opts.dream_store or not resolved_agent_id:
      return []

    observations: list[dict[str, Any]] = []
    runtime = self._create_syscall_runtime()
    action = kernel_action(runtime, observations, {"kind": "query_memory", "query": asdict(query)})
    if action.kind != "query_memory":
      raise RuntimeError(f"query_memory returned unexpected kernel effect: {action.kind}")

    hits: list[Any] = []
    error = None
    try:
      from dataclasses import replace
      hits = await self._opts.dream_store.search(
        resolved_agent_id, replace(query, top_k=int(action.requested_k or 0)),
      )
    except Exception as exc:
      error = exc

    kernel_apply(runtime, observations, {
      "kind": "memory_query_result",
      "effect_id": action.effect_id,
      "hits": [asdict(hit) for hit in hits],
      **({"error": format_tool_error(error)} if error else {}),
    })

    await self._append_memory_syscall_observations(resolved_session_id, observations)
    if error:
      raise error
    await self._log_memory_retrieval_result(resolved_session_id, hits)
    return hits

  async def _log_memory_retrieval_result(
    self,
    session_id: str | None,
    hits: list["MemoryRecall"],
  ) -> None:
    if not session_id:
      return
    # The session-log record is the durable audit artifact; the kernel needs no
    # acknowledgment (the former kernel event was a no-op and was removed).
    await self._opts.session_log.append(session_id, {
      "kind": "memory_retrieval_result",
      "hits": [asdict(hit) for hit in hits],
    })

  def _create_syscall_runtime(self) -> KernelRuntime:
    # M4/G5: only override the cumulative token cap when set, else keep the kernel default.
    _policy_kwargs: dict[str, Any] = dict(
      max_tokens=self._opts.max_tokens,
      max_turns=self._opts.max_turns,
      timeout_ms=self._opts.timeout_ms,
    )
    if self._opts.max_total_tokens is not None:
      _policy_kwargs["max_total_tokens"] = self._opts.max_total_tokens
    runtime = KernelRuntime(LoopPolicy(**_policy_kwargs))
    if self._opts.resource_quota is not None:
      kernel_apply(runtime, [], {
        "kind": "set_resource_quota",
        "quota": _resource_quota_to_kernel(self._opts.resource_quota),
      })
    if self._opts.memory_policy is not None:
      kernel_apply(runtime, [], {
        "kind": "set_memory_policy",
        **_memory_policy_to_kernel(self._opts.memory_policy),
      })
    return runtime

  def _group_budget_request(self, *, include_tokens: bool = True) -> tuple[dict[str, int], dict[str, int]]:
    limits: dict[str, int] = {}
    requested: dict[str, int] = {}
    if include_tokens and self._opts.max_total_tokens is not None:
      limits["tokens"] = self._opts.max_total_tokens
      requested["tokens"] = self._opts.max_total_tokens
    if self._opts.resource_quota is not None:
      max_subagents = _resource_quota_to_kernel(self._opts.resource_quota).get("max_total_subagents")
      if max_subagents is not None:
        limits["subagents"] = int(max_subagents)
        requested["subagents"] = int(max_subagents)
    loop_round = self._opts.run_spec.loop_round if self._opts.run_spec is not None else None
    if loop_round is not None:
      requested["rounds"] = 1
      if loop_round.get("max_rounds") is not None:
        limits["rounds"] = int(loop_round["max_rounds"])
    return limits, requested

  async def _settle_group_budget(
    self,
    scope: GroupBudgetScope,
    *,
    tokens: int,
    subagents: int,
    rounds: int,
  ) -> None:
    reliability = self._opts.kernel_reliability
    retries = reliability.host_effect_retry_attempts if reliability is not None else None
    retries = 3 if retries is None else retries
    for attempt in range(retries + 1):
      try:
        await scope.settle(tokens=tokens, subagents=subagents, rounds=rounds)
        return
      except Exception:
        if attempt >= retries:
          raise

  async def _append_memory_syscall_observations(
    self,
    session_id: str | None,
    observations: list[dict[str, Any]],
  ) -> None:
    if not session_id:
      return
    turn = self._active_kernel.turn() if self._active_kernel else 0
    for obs in observations:
      if obs.get("kind") not in ("memory_written", "memory_queried", "memory_validation_failed"):
        continue
      event = kernel_observation_to_session_event(obs, turn)
      if event:
        await self._opts.session_log.append(session_id, event)

  async def spawn_sub_agent(self, spec: "AgentRunSpec") -> "AsyncIterator[StreamEvent]":
    return self._spawn_sub_agent_impl(spec)

  async def _spawn_sub_agent_impl(self, spec: "AgentRunSpec") -> "AsyncIterator[StreamEvent]":
    """Spawn a sub-agent during an active parent run and feed the result back."""
    from deepstrike.runtime.sub_agent_orchestrator import (
      SubAgentRunContext,
      default_sub_agent_orchestrator,
    )
    from deepstrike.types.agent import (
      AgentProcessChangedObservation,
      agent_run_spec_to_kernel,
      sub_agent_result_to_kernel,
    )

    if self._active_kernel is None or self._current_session_id is None:
      raise RuntimeError("spawn_sub_agent requires an active parent run")

    parent_session_id = self._current_session_id
    runtime = self._active_kernel

    observations = kernel_apply(runtime, self._pending_observations, {
      "kind": "spawn_sub_agent",
      "spec": agent_run_spec_to_kernel(spec),
      "parent_session_id": parent_session_id,
    })
    self._next_archive_start = await self._append_observations(
      parent_session_id, runtime, self._next_archive_start,
    )

    from deepstrike.runtime.sub_agent_orchestrator import _find_spawn_obs

    spawned_obs = _find_spawn_obs(observations)
    if spawned_obs is None:
      raise RuntimeError("spawn_sub_agent did not emit agent_process_changed")

    manifest = AgentProcessChangedObservation(
      agent_id=str(spawned_obs.get("agent_id") or spec.identity.agent_id),
      parent_session_id=str(spawned_obs.get("parent_session_id") or parent_session_id),
      role=str(spawned_obs.get("role") or spec.role),
      isolation=str(spawned_obs.get("isolation") or spec.isolation),
      context_inheritance=str(spawned_obs.get("context_inheritance") or "none"),
      permitted_capability_ids=list(spawned_obs.get("permitted_capability_ids") or []),
      turn=spawned_obs.get("turn"),
      state=str(spawned_obs.get("state") or "running"),
      result_termination=spawned_obs.get("result_termination"),
    )

    orchestrator = self._opts.sub_agent_orchestrator or default_sub_agent_orchestrator
    result = await orchestrator.run(SubAgentRunContext(
      parent_opts=self._opts,
      parent_session_id=parent_session_id,
      spec=spec,
      manifest=manifest,
      session_log=self._opts.session_log,
      harness=self._opts.sub_agent_harness,
    ))

    kernel_apply(runtime, self._pending_observations, {
      "kind": "sub_agent_completed",
      "result": sub_agent_result_to_kernel(result),
    })
    yield DoneEvent(
      iterations=result.result.turns_used,
      total_tokens=result.result.total_tokens_used,
      status=result.result.termination,
    )

  async def bootstrap_workflow(
    self,
    spec: "WorkflowSpec",
    *,
    submitter_agent_id: str | None = None,
  ) -> WorkflowOutcome:
    """M5/G1: bootstrap an *agent-authored* workflow ("the model writes its own harness").

    Unlike :meth:`run_workflow` (the host fires the privileged ``load_workflow``), this routes the
    spec through the agent-reachable ``Syscall::LoadWorkflow`` (the ``submit_workflow`` event): with
    no workflow active the kernel **bootstraps** the DAG; if one is already active it **flattens** the
    spec's nodes onto it (bootstrap-or-flatten — one kernel, one quota, never a workflow stack). Gated
    by the same ``max_workflow_nodes`` backstop as runtime submission. The same shared driver runs it.
    """
    from deepstrike.types.agent import submit_workflow_to_kernel

    if self._active_kernel is None or self._current_session_id is None:
      raise RuntimeError("bootstrap_workflow requires an active parent run")
    initial_event = submit_workflow_to_kernel(spec, self._current_session_id, submitter_agent_id)
    return await self.run_workflow(spec, _initial_event=initial_event)

  async def run_workflow(
    self,
    spec: "WorkflowSpec",
    *,
    # Typed recovered terminal outcomes, including control signals and output.
    resumed_outcomes: list[RecoveredNodeOutcome] | None = None,
    resumed_submissions: list | None = None,
    resumed_submission_bases: list[int] | None = None,
    session_id: str | None = None,
    _initial_event: dict[str, Any] | None = None,
  ) -> WorkflowOutcome:
    """Run a declarative workflow DAG, standalone or mid-run.

    With no active parent run (e.g. a stateless request handler) this auto-bootstraps a kernel
    that owns the DAG — ``start_run`` plus the same governance/quota/attention policies a full
    ``run()`` gets — drives it, and tears it down so the runner is reusable. Called during an
    active ``run()``, it drives the workflow on that kernel instead. ``session_id`` names the
    standalone session (defaults to a fresh uuid); it is ignored when a run is already active.
    """
    bootstrapped = self._active_kernel is None or self._current_session_id is None
    group_budget_scope: GroupBudgetScope | None = None
    standalone_runtime: KernelRuntime | None = None
    try:
      if bootstrapped:
        sid = session_id or f"wf-{uuid.uuid4()}"
        # A standalone workflow reserves its own bounded slice before the kernel schedules any node.
        # Mid-run callers reuse the parent run's already-active reservation.
        if self._opts.run_group is not None:
          g = self._opts.run_group
          # W-N5: tagged "vehicle" — an execution envelope, not a persona (ReactiveSession.resume
          # rebuilds peers only).
          request = self._group_budget_request(include_tokens=False)
          group_budget_scope = await GroupBudgetScope.open(
            g,
            GroupMember(sid, self._opts.agent_id, kind="vehicle"),
            limits=request[0],
            requested=request[1],
          )
          self._active_group_budget_scope = group_budget_scope
        # Resume depends on this fact. Do not dispatch any node until it is durable.
        await self._opts.session_log.append(sid, {
          "kind": "run_started",
          "run_id": str(uuid.uuid4()),
          "goal": f"workflow:{len(spec.nodes)} nodes",
          "criteria": [],
          "agent_id": self._opts.agent_id,
        })
        standalone_runtime = self._bootstrap_workflow_kernel(sid, group_budget_scope)
      outcome = await self._run_workflow_inner(
        spec,
        resumed_outcomes=resumed_outcomes,
        resumed_submissions=resumed_submissions,
        resumed_submission_bases=resumed_submission_bases,
        _initial_event=_initial_event,
      )
      if bootstrapped:
        if standalone_runtime is None:
          raise RuntimeError("standalone workflow lost its kernel runtime")
        terminal = kernel_action(
          standalone_runtime,
          self._pending_observations,
          {"kind": "complete_run"},
        )
        if terminal.kind != "done":
          raise RuntimeError("complete_run did not produce a terminal kernel action")
        await self._append_observations(sid, standalone_runtime, 0)
      return outcome
    finally:
      if bootstrapped:
        try:
          if group_budget_scope is not None and not group_budget_scope.closed:
            await group_budget_scope.release()
        finally:
          self._active_kernel = None
          self._current_session_id = None
          self._pending_observations = []
          self._active_group_budget_scope = None

  def _bootstrap_workflow_kernel(
    self,
    session_id: str,
    group_budget_scope: GroupBudgetScope | None = None,
  ) -> KernelRuntime:
    """Bootstrap a standalone kernel for a host-driven workflow with no active parent run.

    Mirrors ``_execute``'s pre-run setup (governance / attention / scheduler-budget / resource-quota,
    then ``start_run``) so DAG-node spawns are gated and quota'd exactly as a mid-run spawn. The
    caller (``run_workflow``) durably appends ``run_started`` before calling this method, so the
    standalone run can be resumed via ``resume_workflow``.
    """
    self._interrupted = False
    self._cancellation_reason = None
    self._pending_observations = []
    self._current_session_id = session_id
    runtime = self._create_syscall_runtime()
    self._active_kernel = runtime

    # K2: lower governance / attention / scheduler in ONE `configure_run` event (resource-quota is
    # already applied by `_create_syscall_runtime`). Requires the 0.2.30 core that ships `configure_run`.
    os_profile = assert_native_profile(self._opts.os_profile or "native")
    gov_policy = self._opts.governance_policy or os_profile.governance_policy
    governance = {k: v for k, v in governance_policy_to_kernel_event(gov_policy).items() if k != "kind"}
    config: dict[str, Any] = {"governance": governance}
    reliability = _kernel_reliability_to_kernel(self._opts.kernel_reliability)
    if reliability is not None:
      config["reliability"] = reliability
    signal_policy = self._opts.signal_policy or os_profile.signal_policy
    config["signal_policy"] = _signal_policy_to_kernel(signal_policy)
    prompt_budget = _prompt_budget_to_kernel(self._opts.prompt_budget)
    if prompt_budget is not None:
      config["prompt_budget"] = prompt_budget
    if self._opts.context_policy is not None:
      config["context_policy"] = normalize_context_policy_v1(
        context_policy_v1(self._opts.context_policy)
      )
    scheduler_policy = _scheduler_policy_to_kernel(self._opts.scheduler_policy)
    if scheduler_policy is not None:
      config["scheduler_policy"] = scheduler_policy
    if group_budget_scope is not None:
      granted = group_budget_scope.granted
      config["budget_grant"] = {
        "reservation_id": group_budget_scope.reservation_id,
        **({"tokens": granted.tokens} if granted.tokens is not None else {}),
        **({"subagents": granted.subagents} if granted.subagents is not None else {}),
        **({"rounds": granted.rounds} if granted.rounds is not None else {}),
      }
    kernel_apply(runtime, self._pending_observations, {"kind": "configure_run", "config": config})
    # ABI v2 has one lifecycle: a standalone workflow starts a real run before loading its DAG.
    # The initial provider effect is superseded by the workflow load.
    kernel_action(runtime, self._pending_observations, {
      "kind": "start_run",
      "task": {"goal": f"workflow session {session_id}", "criteria": []},
    })
    return runtime

  async def _run_workflow_inner(
    self,
    spec: "WorkflowSpec",
    *,
    resumed_outcomes: list[RecoveredNodeOutcome] | None = None,
    resumed_submissions: list | None = None,
    resumed_submission_bases: list[int] | None = None,
    _initial_event: dict[str, Any] | None = None,
  ) -> WorkflowOutcome:
    """W0-ABI: run a declarative workflow DAG.

    The kernel owns the DAG and gates every node spawn through the syscall trap; this driver
    runs each kernel-emitted batch of nodes in parallel via the sub-agent orchestrator, feeds
    their results back, and loops until the kernel reports one typed terminal outcome per node.

    Args:
        spec: The workflow specification.
        _initial_event: Internal — the kernel event that loads the DAG. Defaults to ``load_workflow``
            (host drive); :meth:`bootstrap_workflow` passes a ``submit_workflow`` event instead so an
            agent-authored spec is bootstrapped through the syscall trap with identical driving.
    """
    import asyncio

    from deepstrike.runtime.sub_agent_orchestrator import (
      SubAgentRunContext,
      default_sub_agent_orchestrator,
    )
    from deepstrike.runtime.session_repair import (
      build_workflow_node_completed_event,
      build_workflow_nodes_submitted_event,
    )
    from deepstrike.types.agent import (
      LoopResult,
      SubAgentResult,
      WorkflowSpawnInfo,
      sub_agent_result_to_kernel,
      submit_workflow_nodes_to_kernel,
      workflow_node_to_manifest,
      workflow_node_to_spec,
      workflow_node_status_from_termination,
      workflow_spec_to_kernel,
    )
    from dataclasses import replace as _dc_replace
    from deepstrike.runtime.output_schema import (
      extract_json_value,
      schema_instruction,
      schema_retry_instruction,
      validate_against_schema,
    )
    from deepstrike.runtime.workflow_control_flow import (
      classify_instruction,
      dependency_outputs_note,
      extract_classify_branch,
      extract_judge_winner,
      extract_loop_continue,
      judge_goal,
      loop_instruction,
    )

    # The public run_workflow wrapper guarantees an active kernel here (bootstrapping one when called
    # standalone), so no guard is needed.
    parent_session_id = self._current_session_id
    runtime = self._active_kernel
    orchestrator = self._opts.sub_agent_orchestrator or default_sub_agent_orchestrator

    # M5/G1: bootstrap_workflow passes a pre-built submit_workflow event; otherwise the host load path.
    if _initial_event is not None:
      load_event: dict[str, Any] = _initial_event
    else:
      load_event = {
        "kind": "load_workflow",
        "spec": workflow_spec_to_kernel(spec),
        "parent_session_id": parent_session_id,
      }
      # Exact typed terminal outcomes plus control-flow signals recovered from the journal.
      if resumed_outcomes and len(resumed_outcomes) > 0:
        load_event["resumed_outcomes"] = [
          {
            "agent_id": r.agent_id,
            "status": r.status,
            "termination": r.termination,
            **({"output": r.output} if r.output is not None else {}),
            **({"classify_branch": r.classify_branch} if r.classify_branch is not None else {}),
            **({"tournament_winner": r.tournament_winner} if r.tournament_winner is not None else {}),
            **({"loop_continue": r.loop_continue} if r.loop_continue is not None else {}),
          }
          for r in resumed_outcomes
        ]
      # R3-1: re-apply recorded runtime submissions so dynamically-appended nodes are reconstructed.
      if resumed_submissions and len(resumed_submissions) > 0:
        load_event["resumed_submissions"] = resumed_submissions
      if resumed_submission_bases and len(resumed_submission_bases) > 0:
        load_event["resumed_submission_bases"] = resumed_submission_bases

    observation_start = len(self._pending_observations)
    initial_action = kernel_maybe_action(runtime, self._pending_observations, load_event)
    observations = self._pending_observations[observation_start:]

    # W-3: persist the agent-authored batch (bootstrap base 0 / flatten base N — the kernel
    # announces BOTH) so an interrupted authored workflow reconstructs on resume; the host never
    # had this spec on the ``bootstrap_workflow`` path, unlike ``run_workflow``.
    if _initial_event is not None and _initial_event.get("kind") == "submit_workflow":
      _submitted = next(
        (o for o in observations if o.get("kind") == "workflow_nodes_submitted"), None
      )
      if _submitted is not None:
        await self._opts.session_log.append(
          parent_session_id,
          build_workflow_nodes_submitted_event(
            turn=runtime.turn(),
            nodes=workflow_spec_to_kernel(spec).get("nodes") or [],
            base_index=_submitted.get("base"),
            submitter_agent_id=_initial_event.get("submitter_agent_id"),
          ),
        )

    # G2: each completed node's output keyed by agent id — a reduce node reads its dependencies'
    # outputs from here. Deps always complete in an earlier round than the reduce node consuming
    # them. W-1: on resume it is pre-seeded from the persisted node outputs, so post-resume
    # dependents still see their (pre-crash) dependencies' outputs.
    outputs: dict[str, str] = {}
    for recovered in resumed_outcomes or []:
      if not recovered.output:
        continue
      content = str(recovered.output.get("content") or "")
      outputs[recovered.agent_id] = content
      outputs[re.sub(r"-i\d+$", "", recovered.agent_id)] = content

    def _run_reduce_node(raw: dict) -> Any:
      from deepstrike.runtime.reducers import resolve_reducer
      from deepstrike._kernel import Message

      def _result(content: str, termination: str) -> Any:
        return SubAgentResult(
          agent_id=raw["agent_id"],
          result=LoopResult(
            termination=termination,
            turns_used=0,
            total_tokens_used=0,
            final_message=Message(role="assistant", content=content),
          ),
        )

      reducer = resolve_reducer(raw["reducer"], self._opts.reducers)
      if reducer is None:
        return _result(f'unknown reducer "{raw["reducer"]}"', "error")
      inputs = [{"agent_id": aid, "output": outputs.get(aid, "")} for aid in raw.get("input_agent_ids", [])]
      try:
        return _result(reducer(inputs), "completed")
      except Exception as exc:  # noqa: BLE001 — a thrown reducer fails the node deterministically
        return _result(f'reducer "{raw["reducer"]}" threw: {exc}', "error")

    async def run_node(raw: dict, budget: dict | None = None) -> Any:
      from deepstrike.types.agent import workflow_budget_note

      # G2: a reduce node runs no LLM — execute the registered pure function over its dependency
      # outputs and feed the result back as an ordinary completion. Deterministic; no agent burned.
      if raw.get("reducer"):
        return _run_reduce_node(raw)

      node = WorkflowSpawnInfo(
        agent_id=raw["agent_id"],
        goal=raw["goal"],
        role=raw["role"],
        isolation=raw["isolation"],
        context_inheritance=raw["context_inheritance"],
        model_hint=raw.get("model_hint"),
        trust=raw.get("trust"),
        output_schema=raw.get("output_schema"),
        # W-N2: the dependency agent ids — a DAG edge carries data, not just ordering.
        input_agent_ids=list(raw.get("input_agent_ids") or []),
        # A#2 v2: marks a loop-iteration spawn — workflow_node_to_spec keys the W-N6 stable
        # session id and the DW-3 loop_round pacing trap off it.
        loop_max_iters=raw.get("loop_max_iters"),
        # M4/G5: without this the per-node token cap never reaches the child run (node parity).
        token_budget=raw.get("token_budget"),
        # W-N7: per-node turn / wall-clock caps (same hops as token_budget).
        max_turns=raw.get("max_turns"),
        max_wall_ms=raw.get("max_wall_ms"),
      )
      base_spec = workflow_node_to_spec(node, parent_session_id)
      manifest = workflow_node_to_manifest(node, parent_session_id)
      # G4: surface remaining workflow budget so a coordinator node can size its submission.
      budget_note = workflow_budget_note(budget)
      # W-N2: every dependent node sees its dependencies' outputs (the kernel sends
      # ``input_agent_ids`` for all dependents; judges/reduce keep their special paths).
      deps_note = dependency_outputs_note(node.input_agent_ids, outputs)

      async def _run(goal: str) -> Any:
        final_goal = "\n\n".join(s for s in (goal, deps_note, budget_note) if s)
        return await orchestrator.run(SubAgentRunContext(
          parent_opts=self._opts,
          parent_session_id=parent_session_id,
          spec=_dc_replace(base_spec, goal=final_goal),
          manifest=manifest,
          session_log=self._opts.session_log,
          harness=self._opts.sub_agent_harness,
          # M5 v2.1: this child IS a workflow node — its `start_workflow` flattens to this kernel.
          is_workflow_node=True,
          # W-N1: trusted workflow nodes run on the parent's execution plane (they carry no grant
          # list by design — filtering on the missing list ran every DAG node TOOL-LESS);
          # quarantined nodes stay deny-all filtered (they read untrusted content).
          tool_access="filtered" if raw.get("trust") == "quarantined" else "inherit",
        ))

      def _text(result: Any) -> str:
        final = result.result.final_message
        return getattr(final, "content", "") if final is not None else ""

      def _with_signal(result: Any, **patch: Any) -> Any:
        return _dc_replace(result, result=_dc_replace(result.result, **patch))

      # A#2 tournament judge: compare two entrants' produced outputs rather than running the node's own
      # goal. Look up both candidates, judge over the controller's criterion, report the winner's id.
      judge = raw.get("judge_match")
      if judge:
        left = outputs.get(judge["left"], "")
        right = outputs.get(judge["right"], "")
        result = await _run(judge_goal(base_spec.goal, left, right))
        winner = extract_judge_winner(_text(result))
        winner_id = judge["right"] if winner == "right" else judge["left"]
        return _with_signal(result, tournament_winner=winner_id)

      # A#2 v2 loop iteration: run the increment under the armed pacing trap (workflow_node_to_spec
      # set ``loop_round``, and the iteration resumes the loop's stable session — transcript-as-
      # carry). DW-3 one vocabulary: the kernel-adjudicated `pace` verb IS the continuation signal
      # (stop → loop_continue=False); the legacy text-sniffed JSON blob survives only as the
      # fallback when no pace decision arrives (stub orchestrators, harness children), where no
      # signal still means "run to max_iters" (v1).
      loop_max = raw.get("loop_max_iters")
      if loop_max is not None:
        m = re.search(r"-i(\d+)$", raw["agent_id"])
        iteration = int(m.group(1)) if m else 0
        result = await _run(f"{base_spec.goal}\n\n{loop_instruction(loop_max, iteration)}")
        pace = getattr(result.result, "pace_decision", None)
        if pace:
          return _with_signal(result, loop_continue=pace.get("action") != "stop")
        cont = extract_loop_continue(_text(result))
        return result if cont is None else _with_signal(result, loop_continue=cont)

      # A#2 classify: run the classifier, then extract the chosen branch label (kernel prunes the rest).
      labels = raw.get("classify_labels") or []
      if labels:
        result = await _run(f"{base_spec.goal}\n\n{classify_instruction(labels)}")
        branch = extract_classify_branch(_text(result), labels)
        return result if branch is None else _with_signal(result, classify_branch=branch)

      schema = node.output_schema
      if not schema:
        return await _run(base_spec.goal)

      # G3: instruct + validate + retry once on mismatch; fail the node if it never conforms.
      max_attempts = self._opts.workflow_schema_validation_attempts
      last: Any = None
      last_errors: list[str] = []
      for attempt in range(1, max_attempts + 1):
        goal = (
          f"{base_spec.goal}\n\n{schema_instruction(schema)}"
          if attempt == 1
          else f"{base_spec.goal}\n\n{schema_retry_instruction(schema, last_errors)}"
        )
        result = await _run(goal)
        final = result.result.final_message
        text = getattr(final, "content", "") if final is not None else ""
        errors = validate_against_schema(extract_json_value(text), schema)
        if not errors:
          return result
        last = result
        last_errors = errors

      reason = (
        f"output_schema validation failed after {max_attempts} attempts: " + "; ".join(last_errors)
      )
      from deepstrike._kernel import Message
      return SubAgentResult(
        agent_id=last.agent_id,
        result=LoopResult(
          termination="error",
          turns_used=last.result.turns_used,
          total_tokens_used=last.result.total_tokens_used,
          final_message=Message(role="assistant", content=reason),
        ),
        submitted_nodes=getattr(last, "submitted_nodes", []),
      )

    def _find_done(obs: list):
      return next((o for o in obs if o.get("kind") == "workflow_completed"), None)

    def _typed_outcome(done_observation: dict | None) -> WorkflowOutcome:
      return WorkflowOutcome(
        node_outcomes=[
          workflow_node_outcome_from_kernel(raw)
          for raw in ((done_observation or {}).get("node_outcomes") or [])
        ],
        outputs=dict(outputs),
      )

    def _accept_spawn(spawn: KernelRunnerAction) -> list[dict[str, Any]]:
      observation_start = len(self._pending_observations)
      continuation = kernel_maybe_action(runtime, self._pending_observations, {
        "kind": "workflow_spawn_result",
        "effect_id": spawn.effect_id,
        "started_agent_ids": [str(node.get("agent_id") or "") for node in (spawn.nodes or [])],
        "failures": [],
      })
      if continuation is not None:
        raise RuntimeError(
          f"workflow spawn acknowledgement returned unexpected effect: {continuation.kind}"
        )
      return self._pending_observations[observation_start:]

    done = _find_done(observations)
    if done is not None:
      if initial_action is not None and initial_action.kind == "call_provider":
        self._workflow_continuation_action = initial_action
      return _typed_outcome(done)
    if initial_action is None:
      return _typed_outcome(None)
    if initial_action.kind != "spawn_workflow":
      raise RuntimeError(f"workflow load returned unexpected kernel effect: {initial_action.kind}")
    nodes = list(initial_action.nodes or [])
    budget = initial_action.budget
    observations = _accept_spawn(initial_action)
    done = _find_done(observations)

    while True:
      if not nodes:
        return _typed_outcome(None)

      # Run the currently-runnable nodes in parallel — each is independent within a round.
      round_budget = budget
      # #2-B-ii: per-node tasks + a concurrent preemption monitor. While the batch is in flight the
      # monitor polls the signal source; a Critical InterruptNow → kernel preempt → AgentPreempted →
      # cancel the matching node's task → CancelledError aborts its in-flight LLM call (asyncio idiom,
      # vs node's AbortSignal). On preempt, stop driving and return the torn-down outcome.
      tasks = {n["agent_id"]: asyncio.create_task(run_node(n, round_budget)) for n in nodes}
      preempt_outcome: list | None = None

      async def _monitor() -> None:
        nonlocal preempt_outcome
        source = self._opts.signal_source
        if source is None:
          return
        while not all(t.done() for t in tasks.values()):
          # O2: injected notes participate in the monitor too, so a host inject_note mid-batch is
          # not stranded until the batch settles (drain order matches _next_inbound_signal).
          delivery = await self._next_inbound_signal()
          if all(t.done() for t in tasks.values()):
            if delivery is not None:
              await delivery.nack()
            break
          if delivery is None:
            await asyncio.sleep(0.005)
            continue
          signal_action = await self._consume_inbound_signal(
            delivery,
            lambda sig: kernel_maybe_action(
              runtime, self._pending_observations, _signal_to_kernel_event(sig)
            ),
          )
          observation_start = len(self._pending_observations)
          if signal_action is not None:
            if signal_action.kind != "preempt_sub_agents":
              raise RuntimeError(
                f"workflow signal returned unexpected effect: {signal_action.kind}"
              )
            for aid in signal_action.agent_ids or []:
              task = tasks.get(aid)
              if task is not None:
                task.cancel()
            continuation = kernel_maybe_action(runtime, self._pending_observations, {
              "kind": "preempt_result",
              "effect_id": signal_action.effect_id,
            })
            if continuation is not None and continuation.kind not in ("call_provider", "done"):
              raise RuntimeError(
                f"workflow preemption returned unexpected effect: {continuation.kind}"
              )
          obs = self._pending_observations[observation_start:]
          preempted = next((o for o in obs if o.get("kind") == "agent_preempted"), None)
          if preempted:
            for aid in preempted.get("agent_ids", []):
              t = tasks.get(aid)
              if t is not None:
                t.cancel()
            wc = next((o for o in obs if o.get("kind") == "workflow_completed"), None)
            preempt_outcome = [
              workflow_node_outcome_from_kernel(raw)
              for raw in ((wc or {}).get("node_outcomes") or [])
            ]
            return

      monitor_task = asyncio.create_task(_monitor())
      results = await asyncio.gather(*tasks.values(), return_exceptions=True)
      monitor_task.cancel()
      try:
        await monitor_task
      except asyncio.CancelledError:
        pass
      if preempt_outcome is not None:
        return WorkflowOutcome(node_outcomes=preempt_outcome, outputs=dict(outputs))
      # No preemption → re-raise any genuine node error (preserve the original gather propagation).
      for _r in results:
        if isinstance(_r, BaseException):
          raise _r

      # Feed completions back one at a time. The run-queue executor can unblock a node's dependents
      # the moment it completes, so each feed may emit its own batch — ACCUMULATE across the round.
      next_nodes: list = []
      done = None
      for result in results:
        # G2: record this node's output so a downstream reduce node can consume it.
        _final = result.result.final_message
        out_text = getattr(_final, "content", "") if _final is not None else ""
        outputs[result.agent_id] = out_text
        # A loop iteration completes under `wf-node{N}-i{k}` but its dependents consume the STABLE
        # node id `wf-node{N}` — alias it so the LAST iteration's output is what dependents see.
        stable_id = re.sub(r"-i\d+$", "", result.agent_id)
        if stable_id != result.agent_id:
          outputs[stable_id] = out_text
        # R3-1: if this node's agent submitted more nodes, append them to the parent DAG BEFORE
        # reporting the node's completion — the workflow is still active (the kernel hasn't seen this
        # node finish), so even a last-node submission keeps the DAG alive.
        if getattr(result, "submitted_nodes", None):
          # G1: stamp the submitting node's agent id so the kernel coerces a quarantined submitter's
          # nodes to quarantined (no topological privilege escalation).
          submit_event = submit_workflow_nodes_to_kernel(
            result.submitted_nodes, getattr(result, "agent_id", None)
          )
          observation_start = len(self._pending_observations)
          submit_action = kernel_maybe_action(runtime, self._pending_observations, submit_event)
          sub_obs = self._pending_observations[observation_start:]
          if submit_action is not None:
            if submit_action.kind != "spawn_workflow":
              raise RuntimeError(
                f"workflow node submission returned unexpected effect: {submit_action.kind}"
              )
            next_nodes.extend(submit_action.nodes or [])
            budget = submit_action.budget or budget
            accepted = _accept_spawn(submit_action)
            submitted_done = _find_done([*sub_obs, *accepted])
            if submitted_done is not None:
              done = submitted_done
          # R3-1: persist the submission (kernel-shape nodes) + the kernel-reported base index
          # so resume can re-apply the batch at the exact original graph position. W-N3: also the
          # submitter, so resume drops batches whose submitter re-runs (it will re-submit).
          _submitted = next(
            (o for o in sub_obs if o.get("kind") == "workflow_nodes_submitted"), None
          )
          if _submitted is not None:
            await self._opts.session_log.append(
              parent_session_id,
              build_workflow_nodes_submitted_event(
                turn=runtime.turn(),
                nodes=submit_event.get("nodes") or [],
                base_index=_submitted.get("base"),
                submitter_agent_id=result.agent_id,
              ),
            )
        observation_start = len(self._pending_observations)
        completion_action = kernel_maybe_action(runtime, self._pending_observations, {
          "kind": "sub_agent_completed",
          "result": sub_agent_result_to_kernel(result),
        })
        obs = self._pending_observations[observation_start:]
        if completion_action is not None:
          if completion_action.kind == "spawn_workflow":
            next_nodes.extend(completion_action.nodes or [])
            budget = completion_action.budget or budget
            obs = [*obs, *_accept_spawn(completion_action)]
          elif completion_action.kind != "call_provider":
            raise RuntimeError(
              f"workflow completion returned unexpected effect: {completion_action.kind}"
            )
          else:
            self._workflow_continuation_action = completion_action
        d = _find_done(obs)
        if d is not None:
          done = d
        # Persist node completion for resume recovery. W-1: the result-borne control signals ride
        # along (a resumed classifier re-prunes; a recorded loop stop is honored) plus the output
        # text (post-resume dependents/reduce still see this node's output).
        await self._opts.session_log.append(
          parent_session_id,
          build_workflow_node_completed_event(
            turn=runtime.turn(),
            agent_id=result.agent_id,
            status=workflow_node_status_from_termination(result.result.termination),
            termination=result.result.termination,
            classify_branch=getattr(result.result, "classify_branch", None),
            tournament_winner=getattr(result.result, "tournament_winner", None),
            loop_continue=getattr(result.result, "loop_continue", None),
            output=_final,
          ),
        )
      if done is not None and not next_nodes:
        return _typed_outcome(done)
      nodes = next_nodes

  async def _drive_authored_workflows(self, runtime: Any, action: Any) -> Any:
    """M5 v2.1: drive the sub-workflow(s) a top-level agent authored via ``start_workflow``.

    Called at the safe point (tool turn resolved → kernel in Reason, not suspended). Each runs in THIS
    kernel (the kernel resumes the reason loop on ``workflow_completed`` — ``finish_workflow`` sets
    phase=Reason), then the outcome is injected as a user message and a fresh ``call_provider`` is
    synthesized from the updated context (the workflow drive consumed its own kernel actions — same
    re-render pattern as the reactive-compact retry path).
    """
    specs = self._pending_authored_workflows
    self._pending_authored_workflows = []
    self._workflow_continuation_action = None
    for spec in specs:
      await self.bootstrap_workflow(spec)
    continuation = self._workflow_continuation_action
    if continuation is None or continuation.kind != "call_provider":
      raise RuntimeError("authored workflow completed without a provider continuation")
    return continuation

  async def resume_workflow(
    self, spec: "WorkflowSpec", *, session_id: str | None = None,
  ) -> WorkflowOutcome:
    """Resume a workflow from a session's completed nodes.

    Reads the session log, extracts completed workflow node records (with their W-1 control
    signals + outputs), and calls run_workflow so the kernel skips those nodes, replays control
    flow (classify prune / loop stop), and the driver re-seeds its outputs map. Pass
    ``session_id`` to resume an interrupted standalone run from a stateless handler; omit it to
    resume the active session.
    """
    from deepstrike.runtime.session_repair import (
      recover_workflow_node_outcomes,
      recover_submitted_workflow_nodes,
    )

    sid = session_id or self._current_session_id
    if sid is None:
      raise RuntimeError("resume_workflow requires an active parent run or an explicit session_id")

    events = await self._opts.session_log.read(sid)
    resumed_outcomes = recover_workflow_node_outcomes(events)
    completed_ids = {r.agent_id for r in resumed_outcomes}
    submissions, bases, submitters = recover_submitted_workflow_nodes(events)
    # W-N3: DROP batches whose submitter did NOT complete — that node re-runs on resume and will
    # re-submit its batch; replaying the logged copy too would duplicate its nodes in the DAG.
    # Exact bases keep later graph indices stable while dropped slots remain inert placeholders.
    if len(submissions) > 0:
      keep = [s is None or s in completed_ids for s in submitters]
      submissions = [sub for sub, k in zip(submissions, keep) if k]
      bases = [b for b, k in zip(bases, keep) if k]
    return await self.run_workflow(
      spec,
      resumed_outcomes=resumed_outcomes,
      resumed_submissions=submissions,
      resumed_submission_bases=bases,
      session_id=sid,
    )

  def interrupt(self, reason: OperationCancellationReason = "user") -> None:
    self._interrupted = True
    self._cancellation_reason = reason
    if self._active_operation is not None and self._active_operation.cancelled is not None:
      self._active_operation.cancelled.set()

  def push_knowledge(
    self,
    content: str,
    tokens: int | None = None,
    *,
    key: str | None = None,
    pinned: bool = False,
  ) -> None:
    """Push content into the durable Knowledge slot (skill definitions, reference artifacts).

    K1: ``key`` gives the entry identity — a same-key push upserts (applied at the next
    compaction/renewal boundary, where the cached system[1] block is rewritten anyway) instead of
    appending a duplicate. ``pinned`` exempts the entry from the knowledge-budget sweep.
    No-op when no run is active.
    """
    if self._active_kernel is None:
      return
    event: dict[str, Any] = {
      "kind": "add_knowledge_message",
      "content": content,
      "tokens": tokens if tokens is not None else max(1, len(content) // 4),
    }
    if key is not None:
      event["key"] = key
    if pinned:
      event["pinned"] = True
    kernel_apply(self._active_kernel, self._pending_observations, event)

  def remove_knowledge(self, key: str) -> None:
    """K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.

    Errs-open: an unknown key is a kernel-side no-op. No-op when no run is active.
    """
    if self._active_kernel is None:
      return
    kernel_apply(self._active_kernel, self._pending_observations, {
      "kind": "remove_knowledge",
      "key": key,
    })

  def deactivate_skill(self, name: str) -> None:
    """K3: host-driven skill deactivation (deliberately no model-facing unload — it invites
    thrash). The toolset re-widens at the next provider call; the skill's knowledge pin drops at
    the next compaction/renewal boundary. A later ``skill(name)`` call re-activates and re-pins
    fresh content. Errs-open: not-active is a kernel-side no-op.
    """
    if self._active_kernel is None:
      return
    kernel_apply(self._active_kernel, self._pending_observations, {
      "kind": "skill_deactivated",
      "name": name,
    })
    # Re-arm the SDK-side push guard so a re-activation re-pins the content.
    self._knowledge_pushed_skills.discard(name)

  def inject_note(self, text: str, urgency: str = "normal") -> None:
    """Push a contextual note into the run's signal stream (the system-reminder channel).

    It drains at the next turn boundary, routes through the kernel attention policy, and — once
    acted on — renders as a ``[SIGNAL] <text>`` line in the volatile state turn plus a durable
    directive. Use it to feed host-detected events back to the model mid-run (e.g. "that write was
    a no-op — stop repeating it") without wiring a full ``SignalSource``. ``urgency`` maps to the
    kernel disposition ladder: ``"normal"`` queues for the next boundary (default), ``"high"``
    soft-interrupts, ``"critical"`` preempts.
    """
    self._injected_signals.append(RuntimeSignal(
      source="custom", signal_type="event", urgency=urgency, payload={"goal": text},
    ))

  def latest_entropy(self) -> "EntropySample | None":
    """The most recent kernel session-entropy sample (one per completed turn), or ``None`` before
    the first boundary. A pull companion to the streamed ``entropy_sample`` events — hosts polling
    from outside the stream (e.g. a heartbeat supervisor) read the latest measurement here."""
    return self._last_entropy_sample

  async def _next_inbound_signal(self) -> "_InboundSignalDelivery | None":
    """Injected-note drain shared with the main loop's per-turn poll: injected notes first (FIFO),
    then the configured ``signal_source`` — one code path so the two channels never drift."""
    if self._injected_signals:
      async def _committed() -> bool:
        return True
      return _InboundSignalDelivery(
        str(uuid.uuid4()), f"injected-{uuid.uuid4()}", 1,
        self._injected_signals.pop(0), _committed, _committed,
      )
    if self._opts.signal_source is None:
      return None
    source = self._opts.signal_source
    claim = await source.claim_signal(self._current_session_id)
    if claim is None:
      return None
    receipt = SignalDeliveryReceipt(claim.delivery_id, claim.lease_token)
    return _InboundSignalDelivery(
      claim.signal_id,
      claim.delivery_id,
      claim.delivery_attempt,
      claim.signal,
      lambda: source.ack_signal(receipt),
      lambda: source.nack_signal(receipt),
    )

  async def _consume_inbound_signal(self, delivery: _InboundSignalDelivery, consume: Callable[[_InboundSignalDelivery], Any]):
    try:
      observation_start = len(self._pending_observations)
      result = consume(delivery)
      dispositions = [
        observation for observation in self._pending_observations[observation_start:]
        if observation.get("kind") == "signal_delivery_disposed"
        and observation.get("delivery_id") == delivery.delivery_id
        and observation.get("attempt") == delivery.delivery_attempt
      ]
      if len(dispositions) != 1:
        raise RuntimeError("kernel did not return the matching signal delivery disposition")
      if not await delivery.ack():
        raise RuntimeError("signal lease was lost before acknowledgement")
      return result
    except BaseException:
      await delivery.nack()
      raise

  def mount_tool(self, schema: dict) -> None:
    """Mount a tool capability on the active run. No-op if not running."""
    if self._active_kernel is not None:
      kernel_apply(self._active_kernel, self._pending_observations, {
        "kind": "capability_command",
        "command": {
          "action": "mount",
          "capability": capability_tool(schema),
          "mounted_by": "sdk:runtime",
          "mount_reason": "dynamic_register",
        },
      })

  def mount_skill(self, name: str, description: str) -> None:
    """Mount a skill capability on the active run. No-op if not running."""
    if self._active_kernel is not None:
      kernel_apply(self._active_kernel, self._pending_observations, {
        "kind": "capability_command",
        "command": {
          "action": "mount",
          "capability": capability_skill(name, description),
          "mounted_by": "sdk:runtime",
          "mount_reason": "dynamic_register",
        },
      })

  def mount_marker(self, kind: str, id: str, description: str) -> None:
    """Mount a generic marker capability (e.g. MCP server) on the active run. No-op if not running."""
    if self._active_kernel is not None:
      kernel_apply(self._active_kernel, self._pending_observations, {
        "kind": "capability_command",
        "command": {
          "action": "mount",
          "capability": capability_marker(kind, id, description),
          "mounted_by": "sdk:runtime",
          "mount_reason": "dynamic_register",
        },
      })

  def unmount_capability(self, kind: str, id: str) -> None:
    """Unmount a capability by kind + id from the active run. No-op if not running."""
    if self._active_kernel is not None:
      kernel_apply(self._active_kernel, self._pending_observations, {
        "kind": "capability_command",
        "command": {
          "action": "unmount",
          "kind": kind,
          "id": id,
        },
      })

  @property
  def execution_plane(self) -> ExecutionPlane:
    return self._plane

  async def run(
    self,
    *,
    goal: str,
    session_id: str | None = None,
    criteria: list[str] | None = None,
    attachments: list[dict] | None = None,
    extensions: dict | None = None,
    inherit_events: list | None = None,
  ) -> AsyncIterator[StreamEvent]:
    sid = session_id or str(uuid.uuid4())
    prior = inherit_events if inherit_events is not None else await self._opts.session_log.read(sid)
    mid_run = _is_mid_run(prior)
    resumed_start = next((entry for entry in reversed(prior) if entry.event.get("kind") == "run_started"), None)
    run_id = resumed_start.event["run_id"] if mid_run and resumed_start is not None else str(uuid.uuid4())
    if not mid_run:
      await self._opts.session_log.append(sid, {
        "kind": "run_started",
        "run_id": run_id,
        "goal": goal,
        "criteria": criteria or [],
        **({"agent_id": self._opts.agent_id} if self._opts.agent_id else {}),
        **({"system_prompt": self._opts.system_prompt} if self._opts.system_prompt else {}),
        **({"attachments": attachments} if attachments else {}),
      })
    try:
      async for evt in self._execute(
        sid, goal, criteria or [], extensions,
        prior if prior else None, mid_run, attachments, run_id,
      ):
        yield evt
    finally:
      await self._close_active_scopes()

  async def wake(
    self,
    session_id: str,
    extensions: dict | None = None,
  ) -> AsyncIterator[StreamEvent]:
    events = await self._opts.session_log.read(session_id)
    if any(e.event.get("kind") == "run_terminal" for e in events):
      return
    start_entry = next((e for e in reversed(events) if e.event.get("kind") == "run_started"), None)
    if start_entry is None:
      raise ValueError(f"No run_started event for session: {session_id}")
    start = start_entry.event
    try:
      async for evt in self._execute(
        session_id,
        start["goal"],
        start.get("criteria", []),
        extensions,
        events,
        True,
        start.get("attachments"),
        start["run_id"],
      ):
        yield evt
    finally:
      await self._close_active_scopes()

  async def _resolve_approval_requests(
    self,
    requests: list[dict[str, Any]],
    runtime: KernelRuntime,
    session_id: str,
  ) -> tuple[list[str], list[str], list[StreamEvent]]:
    from deepstrike.runtime.execution_plane import resolve_permission_request

    approved: list[str] = []
    denied: list[str] = []
    events: list[StreamEvent] = []
    run_ctx = RunContext(on_permission_request=self._opts.on_permission_request)

    for g in requests:
      request = PermissionRequestEvent(
        call_id=g["call_id"],
        tool_name=g["tool"],
        arguments=str(g.get("arguments") or "{}"),
        reason=g.get("reason") if isinstance(g.get("reason"), str) else "",
      )
      events.append(request)
      decision = await resolve_permission_request(request, run_ctx)
      events.append(PermissionResolvedEvent(
        call_id=g["call_id"],
        tool_name=g["tool"],
        approved=decision.approved,
        responder=decision.responder or "host",
        reason=getattr(decision, "reason", None),
      ))
      await self._opts.session_log.append(session_id, {
        "kind": "permission_requested",
        "turn": runtime.turn(),
        "tool": g["tool"],
        "arguments": request.arguments,
        "reason": request.reason,
      })
      await self._opts.session_log.append(session_id, {
        "kind": "permission_resolved",
        "turn": runtime.turn(),
        "approved": decision.approved,
        "responder": decision.responder or "host",
      })
      if decision.approved:
        approved.append(g["call_id"])
      else:
        denied.append(g["call_id"])
        deny_reason = getattr(decision, "reason", None) or "permission denied"
        events.append(ToolDeniedEvent(
          call_id=g["call_id"],
          tool_name=g["tool"],
          reason=deny_reason,
        ))
        events.append(ToolResultEvent(
          call_id=g["call_id"],
          name=g["tool"],
          content=f"permission denied: {deny_reason}",
          is_error=True,
          error_kind="governance_denied",
        ))
        await self._opts.session_log.append(session_id, {
          "kind": "tool_denied",
          "turn": runtime.turn(),
          "call_id": g["call_id"],
          "tool_name": g["tool"],
          "reason": deny_reason,
        })
        await self._opts.session_log.append(session_id, {
          "kind": "tool_completed",
          "turn": runtime.turn(),
          "results": [{
            "call_id": g["call_id"],
            "output": f"permission denied: {deny_reason}",
            "is_error": True,
            "error_kind": "governance_denied",
          }],
        })

    return approved, denied, events

  async def _resolve_read_result(
    self,
    session_id: str,
    args_json: str,
  ) -> tuple[str, bool]:
    """O7: resolve a ``read_result`` meta-tool call to the full text of a previously-evicted tool
    output. Resolution order: (a) the effect-committed on-disk result spool, then (b) a
    session-log scan for the original ``tool_completed`` event carrying that
    ``call_id``. Slices the resolved text by ``[offset, offset + max_bytes)`` (plain string slice —
    "bytes-ish")."""
    call_id = ""
    offset = 0
    max_bytes = 4000
    try:
      args = json.loads(args_json or "{}")
      if isinstance(args.get("call_id"), str):
        call_id = args["call_id"]
      if isinstance(args.get("offset"), (int, float)):
        offset = int(args["offset"])
      if isinstance(args.get("max_bytes"), (int, float)):
        max_bytes = int(args["max_bytes"])
    except Exception:
      pass  # malformed arguments — call_id stays empty, falls through to "not found" below

    full: str | None = None
    from deepstrike.runtime.large_result_spool import LargeResultSpool
    spool = self._opts.result_spool or LargeResultSpool()
    try:
      full = await spool.find_by_call_id(call_id)
    except Exception:
      full = None

    if full is None:
      try:
        events = await self._opts.session_log.read(session_id)
        for entry in events:
          event = entry.event
          if event.get("kind") != "tool_completed":
            continue
          for r in event.get("results", []):
            if getattr(r, "call_id", None) == call_id:
              full = r.output
      except Exception:
        full = None

    if full is None:
      return f'no stored output for call_id "{call_id}"', True

    start = max(0, offset)
    end = min(len(full), start + max(0, max_bytes))
    text_slice = full[start:end]
    return f"[read_result {call_id}: chars {start}–{end} of {len(full)}]\n{text_slice}", False

  async def _execute(
    self,
    session_id: str,
    goal: str,
    criteria: list[str],
    extensions: dict | None,
    prior_events: list[SessionEntry] | None,
    resume_mid_run: bool,
    attachments: list[dict] | None = None,
    run_id: str | None = None,
  ) -> AsyncIterator[StreamEvent]:
    self._interrupted = False
    self._cancellation_reason = None
    self._pending_observations = []
    self._pending_page_out_archives = []
    self._active_page_out_archive = None
    self._current_session_id = session_id
    ext = {**(self._opts.extensions or {}), **(extensions or {})}
    create_run_state = getattr(self._opts.provider, "create_run_state", None)
    provider_state = create_run_state() if callable(create_run_state) else None
    next_compressed_archive_start = _next_archived_seq_start(prior_events)
    self._next_archive_start = next_compressed_archive_start

    # Three-layer policy merge: explicit RuntimeOptions > provider.runtime_policy() > defaults
    _get_policy = getattr(self._opts.provider, "runtime_policy", None)
    provider_policy = _get_policy() if callable(_get_policy) else None
    effective_max_turns  = self._opts.max_turns  or (provider_policy.max_turns  if provider_policy else None) or 25
    effective_timeout_ms = self._opts.timeout_ms or (provider_policy.timeout_ms if provider_policy else None)
    operation = OperationContext(
      run_id=run_id or str(uuid.uuid4()),
      session_id=session_id,
      agent_id=self._opts.agent_id,
      deadline_ms=(int(time.time() * 1000) + effective_timeout_ms) if effective_timeout_ms is not None else None,
    )
    self._active_operation = operation
    task_scope = ManagedTaskScope(operation, self._opts.on_background_task_error)
    self._active_task_scope = task_scope

    _policy_kwargs: dict[str, Any] = dict(
      max_tokens=self._opts.max_tokens,
      max_turns=effective_max_turns,
      timeout_ms=effective_timeout_ms,
    )
    # M4/G5: only override the cumulative token cap when set, else keep the kernel default.
    if self._opts.max_total_tokens is not None:
      _policy_kwargs["max_total_tokens"] = self._opts.max_total_tokens
    policy = LoopPolicy(**_policy_kwargs)
    runtime = KernelRuntime(policy)
    self._active_kernel = runtime

    if self._opts.tokenizer:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_tokenizer",
        "name": self._opts.tokenizer,
      })
    if self._opts.enable_plan_tool is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_plan_tool_enabled",
        "enabled": self._opts.enable_plan_tool,
      })

    kernel_apply(runtime, self._pending_observations, {
      "kind": "set_tools",
      "tools": [tool_schema_to_kernel(schema) for schema in self._plane.schemas()],
    })

    if self._opts.system_prompt:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "add_system_message",
        "content": self._opts.system_prompt,
        "tokens": max(1, len(self._opts.system_prompt) // 4),
      })

    if self._opts.initial_memory:
      for mem in self._opts.initial_memory:
        kernel_apply(runtime, self._pending_observations, {
          "kind": "add_knowledge_message",
          "content": mem,
          "tokens": max(1, len(mem) // 4),
        })

    skill_dir = Path(self._opts.skill_dir) if self._opts.skill_dir else None
    if skill_dir and skill_dir.is_dir():
      from deepstrike.skills.registry import SkillRegistry
      registry = SkillRegistry(str(skill_dir))
      # P1-B: pass the scanned SkillMetadata (incl. `allowed_tools`) straight through — re-constructing
      # it field-by-field previously dropped `allowed_tools`.
      skills = registry.scan()
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_available_skills",
        "skills": [skill_metadata_to_kernel(skill) for skill in skills],
      })

    # P1-B/D: configure stable-core tool ids (always exposed under skill gating).
    if self._opts.stable_core_tool_ids:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_stable_core_tools",
        "tool_ids": list(self._opts.stable_core_tool_ids),
      })

    if self._opts.dream_store and self._opts.agent_id:
      kernel_apply(runtime, self._pending_observations, {"kind": "set_memory_enabled", "enabled": True})
    if self._opts.knowledge_source:
      kernel_apply(runtime, self._pending_observations, {"kind": "set_knowledge_enabled", "enabled": True})
    if self._opts.milestone_contract:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "load_milestone_contract",
        "contract": {
          "phases": [
            {
              "id": p.id,
              "criteria": p.criteria or [],
              "unlocks": p.unlocks or [],
              "required_evidence": p.required_evidence or [],
              **({"verifier": p.verifier} if p.verifier else {}),
            }
            for p in self._opts.milestone_contract.phases
          ]
        }
      })

    max_bytes = runtime.recovery_content_bytes()

    if prior_events:
      from deepstrike.runtime.provider_replay import seed_provider_replay_from_events
      repaired = repair_events_for_recovery(prior_events, max_bytes)
      seed_provider_replay_from_events(self._opts.provider, repaired)
      load_archive = self._opts.compression_store.read if self._opts.compression_store else None
      replayed = await _replay_messages_async(repaired, max_bytes, load_archive)
      kernel_apply(runtime, self._pending_observations, {
        "kind": "preload_history",
        "messages": [message_to_kernel(message) for message in replayed],
      })
      # P1-B B3: rebuild active-skill gating after a wake (active_skills is not snapshotted).
      # `knowledge` isn't snapshotted either (same graceful-reset philosophy) — best-effort re-push
      # the skill's content from its replayed tool_result so the durable copy survives a wake too.
      tool_result_by_call_id: dict[str, str] = {}
      for message in replayed:
        for part in (getattr(message, "content_parts", None) or []):
          if getattr(part, "type", None) == "tool_result":
            tool_result_by_call_id[part.call_id] = part.output
      for message in replayed:
        for tc in (getattr(message, "tool_calls", None) or []):
          if tc.name != "skill":
            continue
          try:
            name = json.loads(tc.arguments or "{}").get("name")
            if not name:
              continue
            activated: dict[str, Any] = {"kind": "skill_activated", "name": name}
            if self._opts.skill_lease_turns is not None:
              activated["lease_turns"] = int(self._opts.skill_lease_turns)
            kernel_apply(runtime, self._pending_observations, activated)
            output = tool_result_by_call_id.get(tc.id)
            if output and name not in self._knowledge_pushed_skills:
              self._knowledge_pushed_skills.add(name)
              # K1: keyed — the kernel-side upsert is the authoritative dedup, so a wake re-push
              # of a skill already pinned live can never double-pin (the in-run set resets with
              # each runner instance; the key does not).
              kernel_apply(runtime, self._pending_observations, {
                "kind": "add_knowledge_message",
                "content": output,
                "tokens": max(1, len(output) // 4),
                "key": f"skill:{name}",
              })
          except Exception:
            pass

    session_start = int(time.time() * 1000)
    start_payload = {
      "kind": "start_run",
      "task": {"goal": goal, "criteria": criteria},
    }
    # P0-A: lower an explicit ``run_spec`` and/or the ``allowed_tool_ids`` profile to the kernel's
    # ``capability_filter`` (reuses the existing run_spec wire — no new ABI). Unset on both => no
    # gating (铁律: no config = old behavior).
    allowed_tool_ids = self._opts.allowed_tool_ids
    has_profile = bool(allowed_tool_ids)
    if self._opts.run_spec or has_profile:
      import dataclasses
      from deepstrike.types.agent import (
        agent_run_spec_to_kernel,
        AgentRunSpec,
        AgentIdentity,
        AgentCapabilityFilter,
      )
      base_spec = self._opts.run_spec or AgentRunSpec(
        identity=AgentIdentity(
          agent_id=self._opts.agent_id or "root", session_id=session_id, is_sub_agent=False
        ),
        role="custom",
        goal=goal,
      )
      if has_profile:
        base_filter = base_spec.capability_filter or AgentCapabilityFilter()
        spec = dataclasses.replace(
          base_spec,
          capability_filter=AgentCapabilityFilter(
            allowed_kinds=base_filter.allowed_kinds,
            allowed_ids=list(allowed_tool_ids),
          ),
        )
      else:
        spec = base_spec
      start_payload["run_spec"] = agent_run_spec_to_kernel(spec)

    os_profile = assert_native_profile(self._opts.os_profile or "native")
    gov_policy = self._opts.governance_policy or os_profile.governance_policy
    kernel_apply(
      runtime,
      self._pending_observations,
      governance_policy_to_kernel_event(gov_policy),
    )

    signal_policy = self._opts.signal_policy or os_profile.signal_policy
    config: dict[str, Any] = {
      "signal_policy": _signal_policy_to_kernel(signal_policy),
    }
    prompt_budget = _prompt_budget_to_kernel(self._opts.prompt_budget)
    if prompt_budget is not None:
      config["prompt_budget"] = prompt_budget
    if self._opts.context_policy is not None:
      config["context_policy"] = normalize_context_policy_v1(
        context_policy_v1(self._opts.context_policy)
      )
    scheduler_policy = _scheduler_policy_to_kernel(self._opts.scheduler_policy)
    if scheduler_policy is not None:
      config["scheduler_policy"] = scheduler_policy
    kernel_apply(runtime, self._pending_observations, {
      "kind": "configure_run",
      "config": config,
    })

    if self._opts.resource_quota is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_resource_quota",
        "quota": _resource_quota_to_kernel(self._opts.resource_quota),
      })

    # O6: tune/disable the in-kernel repeat fuse (absent ⇒ kernel defaults: enabled, 5/8).
    if self._opts.repeat_fuse is not None:
      rf = self._opts.repeat_fuse
      if rf is False:
        payload: dict[str, Any] = {"enabled": False}
      else:
        payload = {"enabled": True}
        if isinstance(rf, dict):
          if rf.get("deny_after") is not None:
            payload["deny_after"] = rf["deny_after"]
          if rf.get("terminate_after") is not None:
            payload["terminate_after"] = rf["terminate_after"]
      kernel_apply(runtime, self._pending_observations, {"kind": "set_repeat_fuse", **payload})

    # O4: turn-end criteria gate toggle (absent ⇒ kernel default: enabled).
    if self._opts.criteria_gate is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_criteria_gate", "enabled": bool(self._opts.criteria_gate),
      })

    # K2: knowledge budget ratio (absent ⇒ kernel default 0.25; 0 disables).
    if self._opts.knowledge_budget_ratio is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_knowledge_budget", "ratio": float(self._opts.knowledge_budget_ratio),
      })

    # Entropy watch (opt-in): threshold alerting over the per-turn session-entropy score.
    # Absent keys keep kernel defaults (threshold 0.65 / hysteresis 0.1 / cooldown 4).
    if self._opts.entropy_watch is not None:
      ew = self._opts.entropy_watch
      payload = {"enabled": bool(ew.get("enabled", True))}
      if ew.get("threshold") is not None:
        payload["threshold"] = float(ew["threshold"])
      if ew.get("hysteresis") is not None:
        payload["hysteresis"] = float(ew["hysteresis"])
      if ew.get("cooldown_turns") is not None:
        payload["cooldown_turns"] = int(ew["cooldown_turns"])
      if ew.get("notify_model") is not None:
        payload["notify_model"] = bool(ew["notify_model"])
      kernel_apply(runtime, self._pending_observations, {"kind": "set_entropy_watch", **payload})

    reliability = _kernel_reliability_to_kernel(self._opts.kernel_reliability)
    if reliability is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "configure_run",
        "config": {"reliability": reliability},
      })

    # Reserve capacity before start_run and give the opaque grant to the kernel. The kernel enforces
    # only local granted capacity and reports exact terminal usage against the same reservation id.
    group_budget_scope: GroupBudgetScope | None = None
    if self._opts.run_group is not None:
      request = self._group_budget_request()
      group_budget_scope = await GroupBudgetScope.open(
        self._opts.run_group,
        GroupMember(session_id, self._opts.agent_id, kind="vehicle"),
        limits=request[0],
        requested=request[1],
      )
      self._active_group_budget_scope = group_budget_scope
      granted = group_budget_scope.granted
      kernel_apply(runtime, self._pending_observations, {
        "kind": "configure_run",
        "config": {"budget_grant": {
          "reservation_id": group_budget_scope.reservation_id,
          **({"tokens": granted.tokens} if granted.tokens is not None else {}),
          **({"subagents": granted.subagents} if granted.subagents is not None else {}),
          **({"rounds": granted.rounds} if granted.rounds is not None else {}),
        }},
      })

    if self._opts.memory_policy is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_memory_policy",
        **_memory_policy_to_kernel(self._opts.memory_policy),
      })

    # Multimodal upload: seed the user's attachments (images/audio) as a history
    # message before start_run pushes the "[TASK STATE]" anchor. init_task does not
    # clear history, so both land in the first render. On resume they are already
    # in the replayed history.
    if not resume_mid_run and attachments:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "add_history_message",
        "message": {"role": "user", "content": list(attachments)},
      })

    # I4: pre-fetch memory before the first LLM turn so the model sees it on turn 1 instead of
    # discovering it via the memory tool on turn 3+ (mirrors Node). Strict dynamic context control:
    # this is single-use retrieval content, not a stable method/skill — so it lands in `history` as
    # an ordinary turn (exactly like a real `memory` tool result would) and decays with the
    # compression pyramid over subsequent turns, instead of pinning itself in `knowledge` forever.
    # K4: the same prefetch re-fires after each sprint renewal (see _prefetch_memory_into_history).
    self._current_goal = goal
    if not resume_mid_run:
      await self._prefetch_memory_into_history(runtime, "initial")

    action = (
      kernel_action(runtime, self._pending_observations, {"kind": "resume"})
      if resume_mid_run
      else kernel_action(runtime, self._pending_observations, start_payload)
    )
    # P0-C: the skill loaded and in effect going into the current turn → per-turn ``active_skill`` metric.
    active_skill: str | None = None

    # I0b: kernel-throw safety net — see Node runner for full rationale.
    try:
     while not runtime.is_terminal():
      next_compressed_archive_start = await self._append_observations(
        session_id, runtime, next_compressed_archive_start, task_scope,
      )
      self._next_archive_start = next_compressed_archive_start
      if self._interrupted:
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "cancel_operation",
          "reason": self._cancellation_reason or "user",
          "pending_call_ids": _pending_call_ids(action),
        })
        break

      if self._opts.signal_source or self._injected_signals:
        delivery = await self._next_inbound_signal()
        if delivery:
          sig_action = await self._consume_inbound_signal(
            delivery,
            lambda sig: kernel_maybe_action(runtime, self._pending_observations, _signal_to_kernel_event(sig)),
          )
          if sig_action:
            action = sig_action
          # I0a: Critical signal carries user_abort intent; see Node runner for full rationale.
          # A critical signal is a kernel attention/preemption decision, not operation cancellation.
      if runtime.is_terminal():
        break

      if action.kind == "call_provider":
        # M5 v2.1: top-level auto-pivot at the safe point (kernel in Reason, not suspended). Loop-top
        # placement catches every path to call_provider (incl. post-approval-resume), so a queued
        # authored spec is never stranded. Drains the queue; fires once per authored batch.
        if self._pending_authored_workflows:
          action = await self._drive_authored_workflows(runtime, action)
        provider_effect_id = action.effect_id
        final_tool_calls: list[ToolCall] = []
        final_text = ""
        context = action.context or RenderedContext()
        # I5: governance schema-level pre-filter — mirrors Node. Tools that the policy denies are
        # dropped from the schema before the provider sees them; the model never tries them.
        turn_tools = action.tools or []
        if self._opts.governance_policy and getattr(self._opts.governance_policy, "surface_denied_in_system", True):
          from deepstrike.governance import governance_filter_schema as _gov_filter
          allowed, denied = _gov_filter(turn_tools, self._opts.governance_policy)
          if denied:
            turn_tools = allowed
            note = f"[governance] the following tools are denied for this run and will fail if called: {', '.join(denied)}."
            existing = getattr(context, "system_knowledge", "") or ""
            try:
              context = type(context)(**{**context.__dict__, "system_knowledge": f"{existing}\n\n{note}".strip()})
            except Exception:
              pass  # don't break the run if the context can't be cloned
        turn_tokens = 0
        turn_input_tokens = 0
        turn_cache_read_tokens = 0
        turn_cache_creation_tokens = 0
        turn_cache_read_by_slot = None
        turn_stop_reason = None
        try:
          async for evt in self._opts.provider.stream(
            context, turn_tools, extensions=ext if ext else None, state=provider_state,
          ):
            # #2-B-ii: a preempting interrupt() stops consuming the live stream immediately; breaking
            # the `async for` closes the provider's async generator → its httpx context exits → the
            # socket aborts. (Workflow preemption uses task.cancel(), which raises CancelledError here
            # — a BaseException, so the `except Exception` below does not swallow it; it propagates.)
            if self._interrupted:
              break
            if getattr(evt, "type", None) == "usage":
              turn_tokens = getattr(evt, "total_tokens", 0)
              # P0-C: capture input + prompt-cache split for the tool-gating hit-rate baseline.
              turn_input_tokens = getattr(evt, "input_tokens", 0) or 0
              turn_cache_read_tokens = getattr(evt, "cache_read_input_tokens", 0) or 0
              turn_cache_creation_tokens = getattr(evt, "cache_creation_input_tokens", 0) or 0
              # I1: per-slot attribution forwarded to TurnMetrics; None on non-Anthropic providers.
              turn_cache_read_by_slot = getattr(evt, "cache_read_input_tokens_by_slot", None)
              # Phase 4: stop_reason drives the kernel's max-output-tokens recovery; keep the last
              # non-empty value seen this turn (the closing usage frame carries it).
              if getattr(evt, "stop_reason", None):
                turn_stop_reason = evt.stop_reason
              continue
            yield evt
            if isinstance(evt, TextDelta):
              final_text += evt.delta
            elif isinstance(evt, ToolCallEvent):
              final_tool_calls.append(ToolCall(
                id=evt.id, name=evt.name, arguments=json.dumps(evt.arguments),
              ))
        except asyncio.CancelledError:
          self._interrupted = True
          self._cancellation_reason = self._cancellation_reason or "user"
          kernel_action(runtime, self._pending_observations, {
            "kind": "cancel_operation",
            "reason": self._cancellation_reason,
            "pending_call_ids": [provider_effect_id],
          })
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start, task_scope,
          )
          raise
        except Exception as exc:
          if self._interrupted:
            # An interrupt raced with an in-flight error — handled as a clean preempt by the
            # post-stream `_interrupted` check below (timeout → kernel rollback), not a provider error.
            pass
          else:
            # Reactive recovery is now a kernel decision. Forward the raw provider error and dispatch
            # whatever the kernel returns: `call_provider` to retry with a freshly compacted context, or
            # `done` to terminate with an honest `ContextOverflow`. The classify + compact + retry +
            # give-up policy lives in the kernel (one place), not duplicated across the four SDK runners.
            # `continue` re-enters the loop: a recovered turn persists its compaction archive via the
            # loop-top _append_observations, and a terminal `done` exits through `is_terminal()`.
            action = kernel_action(runtime, self._pending_observations, {
              "kind": "provider_error",
              "effect_id": provider_effect_id,
              "message": format_tool_error(exc),
            })
            # Withholding (query.ts parity): surface the raw provider error only when the kernel
            # could NOT recover (it returned a terminal). On a recovered retry (call_provider) the
            # error stays hidden, so embedders that terminate on `error` events don't see a phantom
            # failure mid-recovery.
            if getattr(action, "kind", None) == "done":
              yield ErrorEvent(message=format_tool_error(exc))
            continue

        # #2-B-ii: stream aborted (preempt/interrupt) via the break path — end the turn now.
        if self._interrupted:
          action = kernel_action(runtime, self._pending_observations, {
            "kind": "cancel_operation",
            "reason": self._cancellation_reason or "user",
            "pending_call_ids": [provider_effect_id],
          })
          break

        assistant_message = Message(
          role="assistant", content=final_text, tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
        )
        provider_event: dict[str, Any] = {
          "kind": "provider_result",
          "effect_id": provider_effect_id,
          "message": message_to_kernel(assistant_message),
          "now_ms": int(time.time() * 1000),
          **({"stop_reason": turn_stop_reason} if turn_stop_reason else {}),
        }
        action = kernel_action(runtime, self._pending_observations, provider_event)
        from deepstrike.runtime.provider_replay import peek_provider_replay
        provider_replay = peek_provider_replay(self._opts.provider, final_text, final_tool_calls)
        await self._opts.session_log.append(session_id, build_llm_completed_event(
          turn=runtime.turn(),
          content=final_text,
          tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
          provider_replay=provider_replay,
        ))

        # P0-C: per-turn tool-gating telemetry. ``active_skill`` reflects the skill in effect GOING
        # INTO this turn; a ``skill`` call here only takes effect next turn — emit first, then advance.
        if self._opts.on_turn_metrics is not None:
          try:
            _tm_kwargs_by_slot = {"cache_read_tokens_by_slot": turn_cache_read_by_slot} if turn_cache_read_by_slot else {}
            self._opts.on_turn_metrics(TurnMetrics(
              turn=runtime.turn(),
              tools_exposed=len(action.tools or []),
              tools_called=len(final_tool_calls),
              input_tokens=turn_input_tokens,
              cache_read_tokens=turn_cache_read_tokens,
              cache_creation_tokens=turn_cache_creation_tokens,
              active_skill=active_skill,
              **_tm_kwargs_by_slot,
            ))
          except Exception:
            pass  # metrics must never break the run
        skill_call = next((c for c in final_tool_calls if c.name == "skill"), None)
        if skill_call is not None:
          try:
            name = json.loads(skill_call.arguments or "{}").get("name")
            if name:
              active_skill = name
          except Exception:
            pass  # malformed skill args — leave active_skill unchanged

      elif action.kind == "request_approval":
        approved, denied, suspend_events = await self._resolve_approval_requests(
          action.requests or [], runtime, session_id,
        )
        for evt in suspend_events:
          yield evt
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "approval_result",
          "effect_id": action.effect_id,
          "approved_calls": approved,
          "denied_calls": denied,
        })

      elif action.kind == "spool_large_result":
        from deepstrike.runtime.large_result_spool import LargeResultSpool
        spool = self._opts.result_spool or LargeResultSpool()
        spool_ref = None
        error = None
        try:
          spool_ref = await spool.persist_output(action.call_id or "", action.output or "")
        except Exception as exc:
          error = format_tool_error(exc)
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "large_result_spool_result",
          "effect_id": action.effect_id,
          **({"spool_ref": spool_ref} if spool_ref else {}),
          **({"error": error} if error else {}),
        })

      elif action.kind == "archive_page_out":
        if self._active_page_out_archive is None:
          if self._pending_page_out_archives:
            self._active_page_out_archive = self._pending_page_out_archives.pop(0)
          else:
            self._active_page_out_archive = (
              self._next_archive_start,
              await self._opts.session_log.latest_seq(session_id),
            )
        archive_start, _compressed_seq = self._active_page_out_archive
        archive_ref = None
        error = None
        archived = list(action.archived or [])
        try:
          if self._opts.compression_store is not None:
            archive_ref = await self._opts.compression_store.write(session_id, archive_start, archived)
        except Exception as exc:
          error = format_tool_error(exc)
        archive_action = _compression_action(action.action) or "auto_compact"
        archive_tier = action.tier
        if error is None:
          self._active_page_out_archive = None
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "page_out_archive_result",
          "effect_id": action.effect_id,
          **({"archive_ref": archive_ref} if archive_ref else {}),
          **({"error": error} if error else {}),
        })
        if error is None and archive_tier == "semantic" and archived:
          task_scope.spawn("semantic-page-out", self._archive_semantic_page_out(
            archived, archive_action, session_id,
          ))

      elif action.kind == "execute_tool":
        tool_effect_id = action.effect_id
        all_calls = list(action.calls or [])
        await self._opts.session_log.append(session_id, {
          "kind": "tool_requested", "turn": runtime.turn(), "calls": all_calls,
        })
        from deepstrike.runtime.large_result_spool import LargeResultSpool
        run_ctx = RunContext(
          operation=operation,
          agent_id=self._opts.agent_id,
          memory_scope=self._opts.memory_scope,
          skill_dir=skill_dir,
          dream_store=self._opts.dream_store,
          knowledge_source=self._opts.knowledge_source,
          on_tool_suspend=self._opts.on_tool_suspend,
          on_permission_request=self._opts.on_permission_request,
          result_spool=self._opts.result_spool or LargeResultSpool(),
        )
        tool_results: list[ToolResult] = []
        # M5 v1: `start_workflow` (author a sub-workflow) flattens to the same append path.
        normal_calls = [
          c for c in all_calls
          if c.name not in ("update_plan", "submit_workflow_nodes", "start_workflow", "read_result")
        ]
        plan_calls = [c for c in all_calls if c.name == "update_plan"]
        submit_calls = [c for c in all_calls if c.name in ("submit_workflow_nodes", "start_workflow")]
        # O7: `read_result` re-fetches a tool output the kernel evicted from context. Content is
        # host-resolved: (a) this turn's in-memory pending spool map, (b) the on-disk result spool,
        # (c) a session-log scan for the original `tool_completed` event.
        read_result_calls = [c for c in all_calls if c.name == "read_result"]

        for call in plan_calls:
          update = _parse_update_plan_args(call.arguments)
          kernel_apply(runtime, self._pending_observations, {
            "kind": "update_task",
            "update": task_update_to_kernel(update),
          })
          result = ToolResult(call_id=call.id, output="success", is_error=False)
          tool_results.append(result)
          yield ToolResultEvent(call_id=call.id, content="success", is_error=False)

        for call in read_result_calls:
          text, is_error = await self._resolve_read_result(session_id, call.arguments)
          tool_results.append(ToolResult(call_id=call.id, output=text, is_error=is_error))
          yield ToolResultEvent(call_id=call.id, content=text, is_error=is_error)

        # R3-1: `submit_workflow_nodes` cannot be applied to this runner's kernel — when this runner
        # is a workflow node, the workflow lives in the *parent* kernel. Surface the requested nodes
        # as an event; the orchestrator collects them and `run_workflow` sends `submit_workflow_nodes`
        # to the parent kernel. (Outside a workflow node the event is simply unconsumed — a no-op.)
        for call in submit_calls:
          # M5 v2.1: a TOP-LEVEL agent authoring a whole sub-workflow via `start_workflow` — record the
          # spec and AUTO-PIVOT once this tool turn resolves (drive it in this kernel, inject the
          # outcome). A workflow-NODE's `start_workflow` (and every `submit_workflow_nodes`) FLATTENS:
          # the batch is surfaced for the parent `run_workflow` to append.
          if call.name == "start_workflow" and not self._opts.is_workflow_node:
            spec = _parse_start_workflow_spec(call.arguments)
            if spec is not None:
              self._pending_authored_workflows.append(spec)
              out = "workflow authored; executing now"
              tool_results.append(ToolResult(call_id=call.id, output=out, is_error=False))
              yield ToolResultEvent(call_id=call.id, content=out, is_error=False)
              continue
          # `start_workflow` wraps the batch as `{spec: {nodes}}`; `submit_workflow_nodes` is `{nodes}`.
          nodes = (
            _parse_start_workflow_args(call.arguments)
            if call.name == "start_workflow"
            else _parse_submit_workflow_nodes_args(call.arguments)
          )
          yield WorkflowNodesSubmittedEvent(nodes=nodes)
          tool_results.append(ToolResult(call_id=call.id, output="submitted", is_error=False))
          yield ToolResultEvent(call_id=call.id, content="submitted", is_error=False)

        # O5 (PreToolUse-hook analog): give the host a STATEFUL veto over each kernel-approved
        # call. A blocked call never executes; its reason reaches the model as a governance-denied
        # tool result (the kernel rolls the turn back with the note). Decision failures are closed
        # unless the host explicitly marks this hook advisory with ``on_tool_call_failure="open"``.
        executable_calls = normal_calls
        if self._opts.on_tool_call is not None:
          allowed = []
          for call in normal_calls:
            decision = None
            try:
              decision = self._opts.on_tool_call({
                "call_id": call.id, "name": call.name, "arguments": call.arguments,
              })
              if inspect.isawaitable(decision):
                decision = await decision
            except Exception as cause:
              decision = (
                None if self._opts.on_tool_call_failure == "open"
                else {"block": True, "reason": f"on_tool_call hook failed: {format_tool_error(cause)}"}
              )
            if decision and decision.get("block"):
              reason = decision.get("reason") or "blocked by host on_tool_call hook"
              yield ToolDeniedEvent(call_id=call.id, tool_name=call.name, reason=reason)
              await self._opts.session_log.append(session_id, {
                "kind": "tool_denied", "turn": runtime.turn(),
                "call_id": call.id, "tool_name": call.name, "reason": reason,
              })
              out = f"blocked by host hook: {reason}"
              blocked = ToolResult(call_id=call.id, output=out, is_error=True)
              if hasattr(blocked, "error_kind"):
                blocked.error_kind = "governance_denied"
              tool_results.append(blocked)
              yield ToolResultEvent(call_id=call.id, content=out, is_error=True)
              continue
            allowed.append(call)
          executable_calls = allowed

        if executable_calls:
          async for evt in self._plane.execute_all(executable_calls, run_ctx):
            yield evt
            if isinstance(evt, ToolResultEvent):
              result = ToolResult(call_id=evt.call_id, output=evt.content, is_error=evt.is_error)
              if hasattr(result, "is_fatal"):
                result.is_fatal = getattr(evt, "is_fatal", False)
              if hasattr(result, "error_kind"):
                result.error_kind = getattr(evt, "error_kind", None)
              tool_results.append(result)
            elif isinstance(evt, ToolArgumentRepairedEvent):
              await self._opts.session_log.append(session_id, {
                "kind": "tool_argument_repaired",
                "turn": runtime.turn(),
                "tool": evt.name,
                "original_arguments": evt.original_arguments,
                "repaired_arguments": evt.repaired_arguments,
              })
            elif isinstance(evt, ToolDeniedEvent):
              await self._opts.session_log.append(session_id, {
                "kind": "tool_denied",
                "turn": runtime.turn(),
                "call_id": evt.call_id,
                "tool_name": evt.tool_name,
                "reason": evt.reason,
              })
            elif isinstance(evt, PermissionRequestEvent):
              turn = runtime.turn()
              import json as _json
              await self._opts.session_log.append(session_id, {
                "kind": "permission_requested",
                "turn": turn,
                "tool": evt.tool_name,
                "arguments": _json.dumps(evt.arguments) if not isinstance(evt.arguments, str) else evt.arguments,
                "reason": evt.reason,
              })
            elif isinstance(evt, PermissionResolvedEvent):
              turn = runtime.turn()
              await self._opts.session_log.append(session_id, {
                "kind": "permission_resolved",
                "turn": turn,
                "approved": evt.approved,
                "responder": evt.responder,
              })
          names = ", ".join(c.name for c in executable_calls)
          kernel_apply(runtime, self._pending_observations, {
            "kind": "update_task",
            "update": task_update_to_kernel(TaskUpdate(progress=f"Executed tools: {names}")),
          })

        # O5 (PostToolUse-hook analog): let the host inspect each executed result BEFORE it reaches
        # the kernel/session-log — replace the output and/or inject a signal note. Errs-open.
        if self._opts.on_tool_result is not None:
          for r in tool_results:
            call = next((c for c in executable_calls if c.id == r.call_id), None)
            if call is None:
              continue  # plan/submit synthetics and hook-blocked calls are not host results
            decision = None
            try:
              decision = self._opts.on_tool_result({
                "call_id": r.call_id, "name": call.name, "arguments": call.arguments,
                "output": r.output, "is_error": r.is_error,
              })
              if inspect.isawaitable(decision):
                decision = await decision
            except Exception:
              decision = None
            if not decision:
              continue
            if isinstance(decision.get("replace_output"), str):
              r.output = decision["replace_output"]
            if decision.get("note"):
              self.inject_note(str(decision["note"]))

        await self._opts.session_log.append(session_id, {
          "kind": "tool_completed", "turn": runtime.turn(), "results": tool_results,
        })
        # P1-B B3: a successfully-resolved `skill` call activates that skill for the next turn.
        #
        # Strict dynamic context control: a skill is METHOD content — how to do something — reused
        # for the rest of the run, unlike a one-off memory/knowledge lookup (fact content, relevant
        # for the moment it's used). So its text ALSO goes into the durable `knowledge` slot here
        # (in addition to the ordinary tool_result already headed for `history`, where it will decay
        # with the compression pyramid like any other tool output — that's fine, the permanent copy
        # now lives in `knowledge`). First activation only (see `_knowledge_pushed_skills`).
        for call in all_calls:
          if call.name != "skill":
            continue
          res = next((r for r in tool_results if r.call_id == call.id), None)
          if res is None or res.is_error:
            continue
          try:
            name = json.loads(call.arguments or "{}").get("name")
            if not name:
              continue
            activated: dict[str, Any] = {"kind": "skill_activated", "name": name}
            if self._opts.skill_lease_turns is not None:
              activated["lease_turns"] = int(self._opts.skill_lease_turns)
            kernel_apply(runtime, self._pending_observations, activated)
            # With a lease configured, skip the set optimization: an expired-then-reloaded skill
            # must re-pin, and only the kernel knows the lease state — its upsert dedupes anyway.
            if self._opts.skill_lease_turns is not None or name not in self._knowledge_pushed_skills:
              self._knowledge_pushed_skills.add(name)
              # K1: keyed `skill:<name>` — the kernel-side upsert dedupes across runner instances
              # (wake re-push of an already-pinned skill upserts instead of duplicating).
              kernel_apply(runtime, self._pending_observations, {
                "kind": "add_knowledge_message",
                "content": res.output,
                "tokens": max(1, len(res.output) // 4),
                "key": f"skill:{name}",
              })
          except Exception:
            pass
        entropy_obs_start = len(self._pending_observations)
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "tool_results",
          "effect_id": tool_effect_id,
          "results": [tool_result_to_kernel(result) for result in tool_results],
        })
        # Surface the boundary's entropy measurement live (the heartbeat watch source) —
        # the session-log record lands via the normal _append_observations path.
        for obs in self._pending_observations[entropy_obs_start:]:
          if obs.get("kind") == "entropy_sample":
            self._last_entropy_sample = _entropy_sample_from_observation(obs)
            yield EntropySampleEvent(sample=self._last_entropy_sample)
          elif obs.get("kind") == "entropy_alert":
            yield EntropyAlertEvent(
              turn=int(obs.get("turn") or 0),
              score=float(obs.get("score") or 0.0),
              threshold=float(obs.get("threshold") or 0.0),
            )

      elif action.kind == "evaluate_milestone":
        milestone_effect_id = action.effect_id
        milestone_policy = self._opts.milestone_policy or "require_verifier"
        if milestone_policy == "auto_pass":
          from deepstrike.types.agent import milestone_check_result_to_kernel, milestone_check_pass
          action = kernel_action(runtime, self._pending_observations, {
            "kind": "milestone_result",
            "effect_id": milestone_effect_id,
            "result": milestone_check_result_to_kernel(milestone_check_pass(action.phase_id)),
          })
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start, task_scope,
          )
        elif self._opts.on_milestone_evaluate is not None:
          from deepstrike.types.agent import milestone_check_result_to_kernel
          check = self._opts.on_milestone_evaluate({
            "phaseId": action.phase_id,
            "criteria": action.criteria or [],
            "requiredEvidence": action.required_evidence or [],
          })
          if inspect.isawaitable(check):
            check = await check
          action = kernel_action(runtime, self._pending_observations, {
            "kind": "milestone_result",
            "effect_id": milestone_effect_id,
            "result": milestone_check_result_to_kernel(check),
          })
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start, task_scope,
          )
        else:
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start, task_scope,
          )
          turns_used = max(1, runtime.turn())
          await self._opts.session_log.append(session_id, build_run_terminal_event(
            reason="milestone_pending",
            turns_used=turns_used,
            total_tokens=0,
          ))
          if group_budget_scope is not None:
            await group_budget_scope.release()
            self._active_group_budget_scope = None
          await task_scope.drain()
          self._active_kernel = None
          self._active_operation = None
          self._current_session_id = None
          yield DoneEvent(iterations=turns_used, total_tokens=0, status="milestone_pending")
          return

      elif action.kind == "done":
        break
    except Exception as err:
      # I0b: kernel rejection (or any other thrown error inside the loop) is observable here — emit
      # run_terminal so downstream code sees a clean end rather than mid-loop EOF.
      err_msg = format_tool_error(err)
      is_invalid_arg = "invalidarg" in err_msg.lower() or "invalid argument" in err_msg.lower()
      reason = "invalid_arg" if is_invalid_arg else "error"
      yield ErrorEvent(message=err_msg)
      try:
        await self._opts.session_log.append(session_id, build_run_terminal_event(
          reason=reason,
          turns_used=runtime.turn() or 0,
          total_tokens=0,
        ))
      except Exception:
        pass  # session log failure must not mask the original error
      if group_budget_scope is not None:
        await group_budget_scope.release()
        self._active_group_budget_scope = None
      await task_scope.drain()
      yield DoneEvent(iterations=runtime.turn() or 0, total_tokens=0, status=reason)
      self._active_kernel = None
      self._active_operation = None
      self._current_session_id = None
      return

    result = action.result if action.kind == "done" else None
    # I0a: preserve preempt intent when loop exits without clean kernel-done (see Node runner for full rationale).
    status = result.termination if result else "error"
    turns_used = max(1, result.turns_used) if result else (runtime.turn() or 0)
    total_tokens = result.total_tokens_used if result else 0

    next_compressed_archive_start = await self._append_observations(
      session_id, runtime, next_compressed_archive_start, task_scope,
    )
    await self._opts.session_log.append(session_id, build_run_terminal_event(
      reason=status,
      turns_used=turns_used,
      total_tokens=total_tokens,
    ))

    if group_budget_scope is not None and not group_budget_scope.closed:
      raise RuntimeError("kernel terminated without a correlated budget_usage_reported observation")

    if self._opts.dream_store and self._opts.agent_id:
      new_msgs = list(runtime.drain_new_messages())
      if new_msgs:
        try:
          from deepstrike.memory.protocols import SessionData
          from deepstrike.memory.extraction import extract_session_memories
          now_ms = int(time.time() * 1000)
          completed_session = SessionData(
            session_id=session_id,
            agent_id=self._opts.agent_id,
            messages=new_msgs,
            created_at_ms=session_start,
            updated_at_ms=now_ms,
          )
          await self._opts.dream_store.save_session(completed_session)
          if self._opts.memory_scope:
            extracted = await extract_session_memories(
              self._opts.dream_provider or self._opts.provider,
              completed_session,
              self._opts.memory_scope,
              self._opts.dream_system_prompt,
            )
            for memory in extracted:
              await self.write_memory(memory, session_id=session_id, agent_id=self._opts.agent_id)
        except Exception:
          pass

    await task_scope.drain()
    self._active_kernel = None
    self._active_operation = None
    self._current_session_id = None
    yield DoneEvent(
      iterations=turns_used,
      total_tokens=total_tokens,
      status=status,
      # ③ loop-agent: surface the kernel-adjudicated after-round decision to the driver.
      pace_decision=getattr(result, "pace_decision", None) if result else None,
    )

  async def _append_observations(
    self,
    session_id: str,
    runtime: KernelRuntime,
    next_archive_start: int,
    task_scope: ManagedTaskScope | None = None,
  ) -> int:
    turn = runtime.turn()
    preserved_refs = runtime.preserved_refs()
    observations = self._pending_observations
    self._pending_observations = []
    for obs in observations:
      if obs.get("kind") == "page_in_requested":
        continue
      if obs.get("kind") == "budget_usage_reported":
        scope = self._active_group_budget_scope
        if scope is None or obs.get("reservation_id") != scope.reservation_id:
          raise RuntimeError("budget usage report does not match the active reservation")
        await self._settle_group_budget(
          scope,
          tokens=int(obs.get("tokens") or 0),
          subagents=int(obs.get("subagents") or 0),
          rounds=int(obs.get("rounds") or 0),
        )
        self._active_group_budget_scope = None

      # M3: mirror the kernel's journaled recall lifecycle into the durable store so recall
      # history survives across sessions.
      if obs.get("kind") == "memory_recalled" and obs.get("recalls"):
        agent_id = self._opts.agent_id
        record_recall = getattr(self._opts.dream_store, "record_recall", None) if self._opts.dream_store else None
        if agent_id and record_recall is not None:
          from deepstrike.memory.protocols import MemoryRecallLifecycle
          recalls = [
            MemoryRecallLifecycle(
              record_id=str(r["record_id"]),
              recall_count=int(r["recall_count"]),
              last_recalled_at=int(r["last_recalled_at"]),
            )
            for r in obs["recalls"]
          ]
          await record_recall(agent_id, recalls)
      # M4: a recall crossed the promotion threshold. Advisory — surface for the host/model to act.
      if obs.get("kind") == "promotion_suggested" and obs.get("record_id"):
        if self._opts.on_promotion_suggested is not None:
          self._opts.on_promotion_suggested(
            record_id=str(obs["record_id"]),
            recall_count=int(obs.get("recall_count") or 0),
          )

      latest = (
        await self._opts.session_log.latest_seq(session_id)
        if obs.get("kind") == "compressed"
        else None
      )
      event = kernel_observation_to_session_event(
        obs,
        turn,
        next_archive_start=next_archive_start,
        latest_seq=latest,
        preserved_refs=preserved_refs,
        compression_action=_compression_action,
      )
      if not event:
        continue
      compressed_seq = await self._opts.session_log.append(session_id, event)
      if event.get("kind") == "compressed":
        if int(obs.get("archived_count") or 0) > 0:
          self._pending_page_out_archives.append((next_archive_start, compressed_seq))
        next_archive_start = compressed_seq + 1
      # K4: a sprint renewal dropped the old history — including any earlier memory hits — so
      # re-run the pre_query_memory prefetch for the new sprint (live observations only: this
      # consumer sits on the live drain path, same placement as the semantic page-out archival).
      if obs.get("kind") == "renewed":
        await self._prefetch_memory_into_history(runtime, "renewal")
    return next_archive_start

  async def _prefetch_memory_into_history(self, runtime: KernelRuntime, phase: str) -> None:
    """I4 + K4: fetch long-term memory hits for the current goal and land them in ``history`` as
    an ordinary user turn — single-use retrieval content that decays with the compression
    pyramid, never pinned into ``knowledge``. Called once before turn 1 (``phase="initial"``) and
    re-fired after each sprint renewal (``phase="renewal"``): renewal drops the old history
    INCLUDING the earlier memory hits, so the new sprint gets a fresh recall pass. Errs-open.

    The ``phase`` kwarg is passed only when the hook's signature accepts it, so pre-K4 hooks
    (``lambda goal: [...]``) keep working unchanged.
    """
    # P10: recall is default-on (CC session-start recall) — with no hook configured,
    # the goal itself is the query. pre_query_memory stays as the targeting override.
    from deepstrike.memory.protocols import MemoryQuery
    if not (self._opts.dream_store and self._opts.agent_id and self._opts.memory_scope):
      return
    hook = self._opts.pre_query_memory or (lambda goal: [MemoryQuery(
      scope=self._opts.memory_scope, query=goal, top_k=5,
    )])
    try:
      try:
        params = inspect.signature(hook).parameters
        accepts_phase = "phase" in params or any(
          p.kind == inspect.Parameter.VAR_KEYWORD for p in params.values()
        )
      except (TypeError, ValueError):
        accepts_phase = False
      result = hook(goal=self._current_goal, phase=phase) if accepts_phase else hook(goal=self._current_goal)
      if hasattr(result, "__await__"):
        result = await result
      queries = result or []
      lines = []
      for q in queries:
        if not isinstance(q, MemoryQuery) or not q.query.strip():
          continue
        hits = await self._opts.dream_store.search(self._opts.agent_id, q)
        for hit in hits:
          lines.append(f"[memory record_id={hit.record.record_id} trust={hit.record.provenance.trust} score={hit.score:.3f}] {hit.record.content}")
      if lines:
        kernel_apply(runtime, self._pending_observations, {
          "kind": "add_history_message",
          "message": {"role": "user", "content": "\n".join(lines)},
        })
    except Exception:
      pass  # errs-open — a faulty pre-fetch never breaks the run

  async def _archive_semantic_page_out(
    self,
    archived: list[Any],
    action: str | None,
    session_id: str,
  ) -> None:
    if not self._opts.dream_store or not self._opts.agent_id or not self._opts.memory_scope:
      return
    if self._opts.dream_summarizer:
      result = self._opts.dream_summarizer(archived, {"action": action})
      summary = await result if inspect.isawaitable(result) else result
    else:
      summary = await self._summarize_for_long_term_memory(archived)
    # P2 write-funnel: route through the ONE gated write_memory syscall so validation,
    # the rolling write quota, dedup, and the memory_written audit all apply. Score is
    # advisory (0.6) — an automatic summary must never outrank curated content.
    from deepstrike.memory.protocols import MemoryProvenance, MemoryRecord
    now = int(time.time() * 1000)
    name = f"page-out-{now}"
    await self.write_memory(MemoryRecord(
      record_id=f"{self._opts.memory_scope.tenant_id}:{self._opts.memory_scope.namespace}:project:{name}",
      scope=self._opts.memory_scope, name=name, kind="project", content=summary,
      description=f"auto summary of {action or 'compaction'} archive",
      provenance=MemoryProvenance(author="extraction", trust="untrusted", session_id=session_id),
      created_at=now, updated_at=now, confidence=0.6,
    ), session_id=session_id, agent_id=self._opts.agent_id)

  async def _summarize_for_long_term_memory(self, archived: list[Any]) -> str:
    provider = self._opts.dream_provider or self._opts.provider
    transcript = "\n".join(
      f"{getattr(m, 'role', m.get('role') if isinstance(m, dict) else 'unknown')}: "
      f"{getattr(m, 'content', m.get('content') if isinstance(m, dict) else '')}"
      for m in archived
    )
    system_text = "\n\n".join(filter(None, [
      self._opts.dream_system_prompt,
      "Summarize the following conversation for long-term memory. Preserve key facts, decisions, and open questions.",
    ]))
    context = RenderedContext(system_text=system_text, turns=[
      Message(role="user", content=transcript, tool_calls=[]),
    ])
    text = ""
    create_state = getattr(provider, "create_run_state", None)
    state = create_state() if callable(create_state) else None
    async for evt in provider.stream(context, [], state=state):
      if isinstance(evt, TextDelta):
        text += evt.delta
    return text.strip() or transcript[:2000]


def _entropy_sample_from_observation(obs: dict) -> EntropySample:
  """Materialize an ``entropy_sample`` kernel observation into the SDK dataclass."""
  return EntropySample(
    turn=int(obs.get("turn") or 0),
    score=float(obs.get("score") or 0.0),
    score_version=int(obs.get("score_version") or 0),
    rho=float(obs.get("rho") or 0.0),
    repeat_pressure=float(obs.get("repeat_pressure") or 0.0),
    failure_rate=float(obs.get("failure_rate") or 0.0),
    rollbacks_in_window=int(obs.get("rollbacks_in_window") or 0),
    window_turns=int(obs.get("window_turns") or 0),
  )


def _compression_action(action: str | None) -> str | None:
  if action in ("snip_compact", "micro_compact", "context_collapse", "auto_compact"):
    return action
  return None


def _is_mid_run(events: list[SessionEntry]) -> bool:
  # Mid-run ⇔ the LAST run_started has no run_terminal after it. Pairing (not mere
  # presence) matters on multi-round loop sessions: round 1's terminal must not make a
  # crashed round 2 look fresh, and driver-level round_* records must not make a fresh
  # round look interrupted.
  last_started = -1
  last_terminal = -1
  for i, entry in enumerate(events):
    kind = entry.event.get("kind")
    if kind == "run_started":
      last_started = i
    elif kind == "run_terminal":
      last_terminal = i
  return last_started >= 0 and last_started > last_terminal


def _pair_orphan_tool_calls(messages: list[Message]) -> list[Message]:
  """Kernel-consumed meta-tools (e.g. ``pace``) are answered by a synthetic tool result the kernel
  keeps in its OWN history but never emits as a ``tool_completed`` session event (they never reach
  the execution plane). On replay that leaves an assistant ``tool_call`` with no following tool
  result — which strict OpenAI-compatible providers reject. This pass re-pairs such orphans by
  inserting a synthetic tool-result message right after the assistant message.

  Discriminator: only pair an orphan when the run CONTINUED past it (a later non-tool message
  exists). A tail assistant tool_call with nothing after it is a genuinely pending tool the run
  stopped in front of (the wake/recovery case) and must stay unpaired so wake executes it. Pure.
  Mirrors the Node SDK ``pairOrphanToolCalls``.
  """
  def _attr(o: Any, key: str) -> Any:
    return o.get(key) if isinstance(o, dict) else getattr(o, key, None)

  out: list[Message] = []
  n = len(messages)
  for i, m in enumerate(messages):
    out.append(m)
    if m.role != "assistant" or not m.tool_calls:
      continue
    answered: set = set()
    j = i + 1
    while j < n and messages[j].role == "tool":
      for p in (messages[j].content_parts or []):
        if _attr(p, "type") == "tool_result":
          answered.add(_attr(p, "call_id"))
      j += 1
    if j >= n:  # pending tail tool call (wake/recovery) — leave it for wake to execute
      continue
    for c in m.tool_calls:
      cid = _attr(c, "id")
      if cid in answered:
        continue
      part = ContentPartObj(
        type="tool_result", call_id=cid,
        output=f"[{_attr(c, 'name')} handled by kernel]", is_error=False,
      )
      out.append(Message(role="tool", content="", tool_calls=[], content_parts=[part]))
  return out


def _replay_messages(events: list[SessionEntry], max_bytes: int | None = None) -> list[Message]:
  messages: list[Message] = []
  for entry in events:
    e = entry.event
    kind = e.get("kind")
    if kind == "run_started":
      criteria = e.get("criteria", [])
      user_text = (
        f"{e['goal']}\n\nCriteria:\n" + "\n".join(f"{i + 1}. {c}" for i, c in enumerate(criteria))
        if criteria
        else e["goal"]
      )
      messages.append(Message(
        role="user", content=user_text, tool_calls=[],
        token_count=max(1, len(user_text) // 4),
      ))
    elif kind == "compressed":
      summary = e.get("summary")
      if summary:
        system_text = f"[Compressed context: turn {e.get('turn', 0)}]\n{summary}"
        messages.append(Message(
          role="system",
          content=system_text,
          tool_calls=[],
          token_count=max(1, len(system_text) // 4),
        ))
    elif kind == "llm_completed":
      content = sanitize_replay_text(e.get("content", ""), max_bytes)
      messages.append(Message(
        role="assistant",
        content=content,
        tool_calls=e.get("tool_calls", []),
        token_count=e.get("token_count"),
      ))
    elif kind == "tool_completed":
      for r in e.get("results", []):
        output = sanitize_replay_text(r.output, max_bytes)
        part = ContentPartObj(
          type="tool_result",
          call_id=r.call_id,
          output=output,
          is_error=r.is_error,
        )
        messages.append(Message(role="tool", content="", tool_calls=[], content_parts=[part]))
    elif kind == "rollbacked":
      len_val = e.get("checkpoint_history_len", 0)
      if len(messages) > len_val:
        messages = messages[:len_val]
  return _pair_orphan_tool_calls(messages)


async def _replay_messages_async(
  events: list[SessionEntry],
  max_bytes: int | None = None,
  load_archive: Callable[[str], Awaitable[list[Message]]] | None = None,
) -> list[Message]:
  messages: list[Message] = []
  archived_turns = {
    int(entry.event.get("turn") or 0)
    for entry in events
    if entry.event.get("kind") == "page_out"
    and entry.event.get("archive_ref")
    and load_archive is not None
  }
  for entry in events:
    e = entry.event
    kind = e.get("kind")
    if kind == "run_started":
      criteria = e.get("criteria", [])
      user_text = (
        f"{e['goal']}\n\nCriteria:\n" + "\n".join(f"{i + 1}. {c}" for i, c in enumerate(criteria))
        if criteria
        else e["goal"]
      )
      messages.append(Message(
        role="user", content=user_text, tool_calls=[],
        token_count=max(1, len(user_text) // 4),
      ))
    elif kind == "compressed":
      # A committed page-out transaction is replayed from its archive event below. The compressed
      # record remains the durable fallback when no archive was committed.
      if int(e.get("turn") or 0) in archived_turns:
        continue
      summary = e.get("summary")
      if summary:
        system_text = f"[Compressed context: turn {e.get('turn', 0)}]\n{summary}"
        messages.append(Message(
          role="system",
          content=system_text,
          tool_calls=[],
          token_count=max(1, len(system_text) // 4),
        ))
    elif kind == "page_out" and e.get("archive_ref") and load_archive:
      loaded_successfully = False
      try:
        archived_msgs = await load_archive(str(e["archive_ref"]))
        for msg in archived_msgs:
          content = sanitize_replay_text(msg.content, max_bytes)
          messages.append(Message(
            role=msg.role,
            content=content,
            tool_calls=msg.tool_calls,
            token_count=msg.token_count,
            content_parts=msg.content_parts,
          ))
        loaded_successfully = True
      except Exception:
        pass
      if not loaded_successfully:
        summary = e.get("summary")
        if summary:
          system_text = f"[Compressed context: turn {e.get('turn', 0)}]\n{summary}"
          messages.append(Message(
            role="system",
            content=system_text,
            tool_calls=[],
            token_count=max(1, len(system_text) // 4),
          ))
    elif kind == "llm_completed":
      content = sanitize_replay_text(e.get("content", ""), max_bytes)
      messages.append(Message(
        role="assistant",
        content=content,
        tool_calls=e.get("tool_calls", []),
        token_count=e.get("token_count"),
      ))
    elif kind == "tool_completed":
      for r in e.get("results", []):
        output = sanitize_replay_text(r.output, max_bytes)
        part = ContentPartObj(
          type="tool_result",
          call_id=r.call_id,
          output=output,
          is_error=r.is_error,
        )
        messages.append(Message(role="tool", content="", tool_calls=[], content_parts=[part]))
    elif kind == "rollbacked":
      len_val = e.get("checkpoint_history_len", 0)
      if len(messages) > len_val:
        messages = messages[:len_val]
  return _pair_orphan_tool_calls(messages)


def _next_archived_seq_start(events: list[SessionEntry] | None) -> int:
  next_seq = 0
  for entry in events or []:
    event = entry.event
    if event.get("kind") == "compressed":
      next_seq = max(next_seq, int(event["archived_seq_range"][1]) + 1)
  return next_seq


def _resource_quota_to_kernel(quota: ResourceQuota | dict[str, Any]) -> dict[str, Any]:
  if isinstance(quota, dict):
    max_concurrent = quota.get("max_concurrent_subagents")
    max_total = quota.get("max_total_subagents")
    max_depth = quota.get("max_spawn_depth")
    rate = quota.get("memory_writes_per_window")
  else:
    max_concurrent = quota.max_concurrent_subagents
    max_total = quota.max_total_subagents
    max_depth = quota.max_spawn_depth
    rate = quota.memory_writes_per_window

  out: dict[str, Any] = {}
  if max_concurrent is not None:
    out["max_concurrent_subagents"] = max_concurrent
  if max_total is not None:
    out["max_total_subagents"] = max_total
  if max_depth is not None:
    out["max_spawn_depth"] = max_depth
  if rate is not None:
    if isinstance(rate, dict):
      out["memory_writes_per_window"] = [
        rate.get("max_writes"),
        rate.get("window_ms"),
      ]
    elif isinstance(rate, MemoryWriteRateLimit):
      out["memory_writes_per_window"] = [rate.max_writes, rate.window_ms]
    else:
      max_writes, window_ms = rate
      out["memory_writes_per_window"] = [max_writes, window_ms]
  return out


def _memory_policy_to_kernel(policy: MemoryPolicy | dict[str, Any]) -> dict[str, Any]:
  """Map the ergonomic snake_case policy onto the flat `set_memory_policy` event fields.

  Omitted (None) fields are dropped so the kernel applies its serde defaults.
  """
  if isinstance(policy, dict):
    get = policy.get
  else:
    get = lambda k: getattr(policy, k)  # noqa: E731
  out: dict[str, Any] = {}
  for field in (
    "memory_path",
    "stale_warning_days",
    "retrieval_top_k",
    "validation_enabled",
    "max_content_bytes",
    "max_name_length",
  ):
    value = get(field)
    if value is not None:
      out[field] = value
  return out


def _scheduler_policy_to_kernel(
  policy: SchedulerPolicy | dict[str, Any] | None,
) -> dict[str, Any] | None:
  if policy is None:
    return None
  if isinstance(policy, dict):
    allowed = {
      "version", "critical_path_weight", "fanout_weight", "age_weight", "token_cost_weight",
    }
    unknown = set(policy) - allowed
    if unknown:
      raise ValueError(f"unknown scheduler policy field(s): {', '.join(sorted(unknown))}")
  get = policy.__getitem__ if isinstance(policy, dict) else lambda key: getattr(policy, key)
  out = {
    "version": get("version"),
    "critical_path_weight": get("critical_path_weight"),
    "fanout_weight": get("fanout_weight"),
    "age_weight": get("age_weight"),
    "token_cost_weight": get("token_cost_weight"),
  }
  return out


def _kernel_reliability_to_kernel(
  policy: KernelReliability | dict[str, Any] | None,
) -> dict[str, Any] | None:
  if policy is None:
    return None
  get = policy.get if isinstance(policy, dict) else lambda key: getattr(policy, key)
  out: dict[str, Any] = {}
  for field in (
    "event_replay_capacity",
    "completed_effect_replay_capacity",
    "provider_recovery_attempts",
    "output_recovery_attempts",
    "host_effect_retry_attempts",
    "spool_threshold_bytes",
    "spool_preview_bytes",
    "snapshot_input_limit",
    "max_input_bytes",
    "snapshot_journal_bytes_limit",
  ):
    value = get(field)
    if value is not None:
      out[field] = value
  return out


def _signal_policy_to_kernel(policy: "SignalPolicy | dict[str, Any]") -> dict[str, Any]:
  queue_max = policy.get("queue_max") if isinstance(policy, dict) else policy.queue_max
  ttl_ms = policy.get("ttl_ms") if isinstance(policy, dict) else policy.ttl_ms
  deadline_escalation = (
    policy.get("deadline_escalation") if isinstance(policy, dict) else policy.deadline_escalation
  )
  return {
    "version": 1,
    "queue_max": queue_max,
    **({"ttl_ms": ttl_ms} if ttl_ms is not None else {}),
    **({"deadline_escalation": deadline_escalation} if deadline_escalation is not None else {}),
  }


def _prompt_budget_to_kernel(
  budget: PromptBudget | dict[str, Any] | None,
) -> dict[str, Any] | None:
  if budget is None:
    return None
  if isinstance(budget, dict):
    return {
      "prompt_overhead_tokens": budget["prompt_overhead_tokens"],
      "output_reserve_tokens": budget["output_reserve_tokens"],
      "safety_margin_tokens": budget["safety_margin_tokens"],
    }
  return {
    "prompt_overhead_tokens": budget.prompt_overhead_tokens,
    "output_reserve_tokens": budget.output_reserve_tokens,
    "safety_margin_tokens": budget.safety_margin_tokens,
  }


def _to_kernel_message(message: object) -> Message:
  if isinstance(message, Message):
    return message
  role = getattr(message, "role", "user")
  content = getattr(message, "content", "")
  token_count = getattr(message, "token_count", None)
  tool_calls = getattr(message, "tool_calls", None) or []
  return Message(role=role, content=content, token_count=token_count, tool_calls=tool_calls)


def _memory_record_from_mapping(value: dict[str, Any]) -> "MemoryRecord":
  from deepstrike.memory.protocols import MemoryProvenance, MemoryRecord, MemoryScope
  scope = value["scope"]
  provenance = value["provenance"]
  return MemoryRecord(
    record_id=str(value["record_id"]),
    scope=MemoryScope(tenant_id=str(scope["tenant_id"]), namespace=str(scope["namespace"])),
    name=str(value["name"]), kind=value["kind"], content=str(value["content"]),
    description=str(value["description"]),
    provenance=MemoryProvenance(
      author=provenance["author"], trust=provenance["trust"],
      evidence_refs=list(provenance.get("evidence_refs") or []),
      session_id=provenance.get("session_id"),
    ),
    created_at=int(value["created_at"]), updated_at=int(value["updated_at"]),
    last_recalled_at=value.get("last_recalled_at"), recall_count=int(value.get("recall_count") or 0),
    confidence=float(value["confidence"]), links=list(value.get("links") or []),
    pinned=bool(value.get("pinned")), ttl_days=value.get("ttl_days"),
  )


async def collect_text(stream: AsyncIterator[StreamEvent]) -> str:
  text = ""
  async for evt in stream:
    if isinstance(evt, TextDelta):
      text += evt.delta
  return text


def _parse_update_plan_args(args_str: str) -> TaskUpdate:
  try:
    parsed = json.loads(args_str)
  except Exception:
    parsed = {}
  plan = parsed.get("plan")
  current_step = parsed.get("current_step")
  if current_step is None:
    current_step = parsed.get("currentStep")
  progress = parsed.get("progress")
  scratchpad = parsed.get("scratchpad")
  blocked_on = parsed.get("blocked_on")
  if blocked_on is None:
    blocked_on = parsed.get("blockedOn")
  return TaskUpdate(
    plan=plan,
    current_step=current_step,
    progress=progress,
    scratchpad=scratchpad,
    blocked_on=blocked_on,
  )


def _nodes_from_raw_list(raw) -> list:
  """Build WorkflowNodeSpecs from a raw node list (shared by the submit + start parsers). Node shapes
  are trusted structurally; the kernel validates them (dep range, quota) on append."""
  from deepstrike.types.agent import WorkflowNodeSpec
  if not isinstance(raw, list):
    return []
  nodes = []
  for item in raw:
    if isinstance(item, dict) and "task" in item and "role" in item:
      nodes.append(WorkflowNodeSpec(
        task=item["task"],
        role=item["role"],
        isolation=item.get("isolation", "shared"),
        context_inheritance=item.get("context_inheritance", "none"),
        model_hint=item.get("model_hint"),
        trust=item.get("trust", "trusted"),
        output_schema=item.get("output_schema"),
        # M2/G2: pass the control-flow kind through so a submitted node can itself be a
        # reduce / loop / classify / tournament (not silently downgraded to a plain spawn).
        reducer=item.get("reducer"),
        loop=item.get("loop"),
        classify=item.get("classify"),
        tournament=item.get("tournament"),
        # M4/G5: pass the per-node token cap through.
        token_budget=item.get("token_budget"),
        depends_on=list(item.get("depends_on") or []),
      ))
  return nodes


def _parse_submit_workflow_nodes_args(args_str: str) -> list:
  """R3-1: parse the ``submit_workflow_nodes`` tool args (``{"nodes": [...]}``) into WorkflowNodeSpec.
  A malformed payload yields no nodes rather than raising."""
  try:
    parsed = json.loads(args_str)
  except Exception:
    return []
  return _nodes_from_raw_list(parsed.get("nodes") if isinstance(parsed, dict) else None)


def _parse_start_workflow_args(args_str: str) -> list:
  """M5 v1: parse the ``start_workflow`` tool args (``{"spec": {"nodes": [...]}}``) into the spec's
  node batch — flattened onto the running workflow via the same append path."""
  try:
    parsed = json.loads(args_str)
  except Exception:
    return []
  spec = parsed.get("spec") if isinstance(parsed, dict) else None
  return _nodes_from_raw_list(spec.get("nodes") if isinstance(spec, dict) else None)


def _parse_start_workflow_spec(args_str: str):
  """M5 v2.1: parse the full ``WorkflowSpec`` from a top-level ``start_workflow`` call for the
  auto-pivot drive. Returns ``None`` on a malformed / empty payload (caller falls back to flatten)."""
  from deepstrike.types.agent import WorkflowSpec

  nodes = _parse_start_workflow_args(args_str)
  return WorkflowSpec(nodes=nodes) if nodes else None


def _authored_workflow_outcome_note(outcome: WorkflowOutcome) -> str:
  """M5 v2.1: render an authored-workflow outcome into a user-message note injected back into the
  agent's context, so its next turn continues with the sub-workflow's results in view."""
  counts: dict[str, int] = {}
  for node in outcome.node_outcomes:
    counts[node.status] = counts.get(node.status, 0) + 1
  lines = [
    f"[authored workflow result] {len(outcome.node_outcomes)} terminal node(s): "
    + ", ".join(f"{count} {status}" for status, count in counts.items()) + "."
  ]
  for node in outcome.node_outcomes:
    out = outcome.outputs.get(node.node_id)
    if not out and node.output:
      out = str(node.output.get("content") or "")
    if out:
      lines.append(
        f"- {node.node_id} ({node.status}): {out[:500] + '…' if len(out) > 500 else out}"
      )
  return "\n".join(lines)


def _signal_to_kernel_event(delivery: _InboundSignalDelivery) -> dict:
  """Lower a claimed host delivery to the kernel's ``deliver_signal`` event. Shared by the main loop's
  per-turn poll and #2-B-ii's workflow-batch preemption monitor (so the two never drift)."""
  sig = delivery.signal
  return {
    "kind": "deliver_signal",
    "delivery_id": delivery.delivery_id,
    "attempt": delivery.delivery_attempt,
    "signal": {
      "id": delivery.signal_id,
      "source": sig.source,
      "signal_type": sig.signal_type,
      "urgency": sig.urgency,
      "summary": str(sig.payload.get("goal") or "signal"),
      "payload": sig.payload,
      **({"dedupe_key": sig.dedupe_key} if sig.dedupe_key else {}),
      **({"recipient": sig.recipient} if getattr(sig, "recipient", None) else {}),
      **({"deadline_ms": sig.deadline_ms} if getattr(sig, "deadline_ms", None) is not None else {}),
      **({"coalesce_key": sig.coalesce_key} if getattr(sig, "coalesce_key", None) else {}),
      "coalesced_count": max(1, getattr(sig, "coalesced_count", 1)),
      "timestamp_ms": int(time.time() * 1000),
    },
  }
