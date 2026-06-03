from __future__ import annotations

import json
import time
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any, Awaitable, Callable

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
)
from deepstrike.runtime.execution_plane import ExecutionPlane, LocalExecutionPlane, RunContext
from deepstrike.governance import governance_policy_to_kernel_event
from deepstrike.runtime.kernel_event_log import kernel_observation_to_session_event, with_category
from deepstrike.runtime.kernel_step import (
  capability_marker,
  capability_skill,
  capability_tool,
  force_compact,
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
  build_llm_completed_event,
  build_run_terminal_event,
  repair_events_for_recovery,
)
from deepstrike.runtime.session_log import SessionEntry, SessionEvent, SessionLog
from deepstrike.runtime.archive import ArchiveStore
from deepstrike.runtime.os_profile import assert_native_profile

if TYPE_CHECKING:
  from deepstrike.governance import Governance, GovernancePolicy
  from deepstrike.runtime.os_profile import AttentionPolicy, OsProfile
  from deepstrike.knowledge.source import KnowledgeSource
  from deepstrike.memory.protocols import DreamResult, DreamStore
  from deepstrike.signals.types import SignalSource
  from deepstrike.types.agent import AgentRunSpec, SubAgentResult, MilestonePolicy, MilestoneContract, MilestoneCheckResult
  from deepstrike.runtime.large_result_spool import LargeResultSpool


@dataclass
class SubAgentHarnessConfig:
  """When set on RuntimeOptions, spawned sub-agents run through HarnessLoop."""
  eval_provider: LLMProvider
  max_attempts: int = 3


@dataclass
class MemoryWriteRateLimit:
  """Rolling-window memory-write rate limit for ResourceQuota."""
  max_writes: int
  window_ms: int


@dataclass
class ResourceQuota:
  """M2 resource quotas installed through the kernel JSON event ABI."""
  max_concurrent_subagents: int | None = None
  max_spawn_depth: int | None = None
  memory_writes_per_window: MemoryWriteRateLimit | tuple[int, int] | None = None


@dataclass
class SchedulerBudget:
  """Optional scheduler budget overrides installed through the kernel JSON event ABI."""
  max_wall_ms: int | None = None


@dataclass
class RuntimeOptions:
  provider: LLMProvider
  session_log: SessionLog
  execution_plane: ExecutionPlane | None = None
  compression_store: ArchiveStore | None = None
  max_tokens: int = 32_000
  max_turns: int = 25
  timeout_ms: int | None = None
  agent_id: str | None = None
  system_prompt: str | None = None
  initial_memory: list[str] | None = None
  skill_dir: str | Path | None = None
  dream_store: "DreamStore | None" = None
  knowledge_source: "KnowledgeSource | None" = None
  signal_source: "SignalSource | None" = None
  extensions: dict | None = None
  governance: "Governance | None" = None
  governance_policy: "GovernancePolicy | None" = None
  attention_policy: "AttentionPolicy | dict | None" = None
  scheduler_budget: SchedulerBudget | dict[str, Any] | None = None
  resource_quota: ResourceQuota | dict[str, Any] | None = None
  os_profile: "OsProfile | None" = None
  tokenizer: str | None = None
  enable_plan_tool: bool | None = None
  on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None
  on_permission_request: Callable[[PermissionRequestEvent], Awaitable[PermissionResponse | bool | dict[str, Any]] | PermissionResponse | bool | dict[str, Any]] | None = None
  sub_agent_orchestrator: Any | None = None
  sub_agent_harness: SubAgentHarnessConfig | None = None
  dream_system_prompt: str | None = None
  milestone_policy: "MilestonePolicy | None" = None
  on_milestone_evaluate: Callable[[dict[str, Any]], Awaitable[Any] | Any] | None = None
  milestone_contract: "MilestoneContract | None" = None
  run_spec: "AgentRunSpec | None" = None
  result_spool: "LargeResultSpool | None" = None
  dream_provider: LLMProvider | None = None
  dream_summarizer: Callable[[list[Any], dict[str, Any]], Awaitable[str] | str] | None = None


class RuntimeRunner:
  def __init__(self, opts: RuntimeOptions) -> None:
    self._opts = opts
    self._interrupted = False
    self._plane = opts.execution_plane or LocalExecutionPlane()
    self._active_kernel: KernelRuntime | None = None
    self._pending_observations: list[dict] = []
    self._current_session_id: str | None = None
    self._next_archive_start: int = 0
    self._pending_spool_outputs: dict[str, dict[str, str]] = {}
    self._local_page_out_cache: list[Any] = []

  @property
  def host_options(self) -> RuntimeOptions:
    """Host configuration (for coordinator / sub-agent spawn)."""
    return self._opts

  async def write_memory(
    self,
    memory: dict[str, Any],
    *,
    session_id: str | None = None,
    agent_id: str | None = None,
  ) -> None:
    resolved_session_id = session_id or self._current_session_id
    resolved_agent_id = agent_id or self._opts.agent_id
    if not self._opts.dream_store or not resolved_agent_id:
      return

    observations: list[dict[str, Any]] = []
    runtime = self._active_kernel or self._create_syscall_runtime()
    kernel_apply(runtime, observations, {"kind": "write_memory", "memory": memory})

    if any(o.get("kind") == "memory_written" for o in observations):
      from deepstrike.memory.protocols import CurationResult, CurationStats, MemoryEntry
      existing = await self._opts.dream_store.load_memories(resolved_agent_id)
      await self._opts.dream_store.commit(
        resolved_agent_id,
        CurationResult(
          to_add=[
            MemoryEntry(
              text=str(memory.get("content") or ""),
              score=1.0,
              metadata={
                **(memory.get("metadata") if isinstance(memory.get("metadata"), dict) else {}),
                "source": "write_memory_syscall",
              },
            )
          ],
          to_remove_indices=[],
          stats=CurationStats(insights_processed=1, entries_added=1),
        ),
        existing,
      )
    await self._append_memory_syscall_observations(resolved_session_id, observations)

  async def query_memory(
    self,
    query: dict[str, Any],
    *,
    session_id: str | None = None,
    agent_id: str | None = None,
  ) -> list[Any]:
    from deepstrike.memory.agent import memories_to_index, select_memories

    resolved_session_id = session_id or self._current_session_id
    resolved_agent_id = agent_id or self._opts.agent_id
    if not self._opts.dream_store or not resolved_agent_id:
      return []

    observations: list[dict[str, Any]] = []
    runtime = self._active_kernel or self._create_syscall_runtime()
    kernel_apply(runtime, observations, {"kind": "query_memory", "query": query})

    all_memories = await self._opts.dream_store.load_memories(resolved_agent_id)
    retrieval = await select_memories(query, memories_to_index(all_memories))
    selected_ids = set(retrieval.get("selected_memory_ids") or [])
    hits: list[Any] = []
    if selected_ids:
      for entry in all_memories:
        meta = entry.metadata if hasattr(entry, "metadata") else {}
        name = meta.get("name") if isinstance(meta, dict) else None
        if name in selected_ids:
          hits.append(entry)
      hits = hits[: int(query.get("top_k") or 5)]
    else:
      hits = await self._opts.dream_store.search(
        resolved_agent_id,
        str(query.get("current_context") or ""),
        int(query.get("top_k") or 5),
      )
      if hits and retrieval.get("selection_rationale") == "No candidates after filtering":
        retrieval["selected_memory_ids"] = [
          (entry.metadata.get("name") if hasattr(entry, "metadata") and isinstance(entry.metadata, dict) else None)
          or getattr(entry, "text", "")[:32]
          for entry in hits
        ]
        retrieval["selection_rationale"] = f"DreamStore.search returned {len(hits)} hit(s)"

    await self._append_memory_syscall_observations(resolved_session_id, observations)
    await self._log_memory_retrieval_result(resolved_session_id, runtime, retrieval)
    return hits

  async def _log_memory_retrieval_result(
    self,
    session_id: str | None,
    runtime: KernelRuntime,
    retrieval: dict[str, Any],
  ) -> None:
    if not session_id:
      return
    await self._opts.session_log.append(session_id, {
      "kind": "memory_retrieval_result",
      "selected_memory_ids": list(retrieval.get("selected_memory_ids") or []),
      "selection_rationale": str(retrieval.get("selection_rationale") or ""),
    })
    try:
      kernel_apply(runtime, [], {
        "kind": "memory_retrieval_result",
        "retrieval": {
          "selected_memory_ids": list(retrieval.get("selected_memory_ids") or []),
          "selection_rationale": str(retrieval.get("selection_rationale") or ""),
        },
      })
    except ValueError:
      # Native extension may lag core ABI; session log is the audit source of truth.
      pass

  def _create_syscall_runtime(self) -> KernelRuntime:
    runtime = KernelRuntime(LoopPolicy(
      max_tokens=self._opts.max_tokens,
      max_turns=self._opts.max_turns,
      timeout_ms=self._opts.timeout_ms,
    ))
    if self._opts.resource_quota is not None:
      kernel_apply(runtime, [], {
        "kind": "set_resource_quota",
        "quota": _resource_quota_to_kernel(self._opts.resource_quota),
      })
    return runtime

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

  def interrupt(self) -> None:
    self._interrupted = True

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
    extensions: dict | None = None,
    inherit_events: list | None = None,
  ) -> AsyncIterator[StreamEvent]:
    sid = session_id or str(uuid.uuid4())
    prior = inherit_events if inherit_events is not None else await self._opts.session_log.read(sid)
    mid_run = _is_mid_run(prior)
    if not mid_run:
      await self._opts.session_log.append(sid, {
        "kind": "run_started",
        "run_id": str(uuid.uuid4()),
        "goal": goal,
        "criteria": criteria or [],
        **({"agent_id": self._opts.agent_id} if self._opts.agent_id else {}),
        **({"system_prompt": self._opts.system_prompt} if self._opts.system_prompt else {}),
      })
    async for evt in self._execute(
      sid, goal, criteria or [], extensions,
      prior if prior else None, mid_run,
    ):
      yield evt

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
    async for evt in self._execute(
      session_id,
      start["goal"],
      start.get("criteria", []),
      extensions,
      events,
      True,
    ):
      yield evt

  async def dream(self, agent_id: str, now_ms: int | None = None) -> "AsyncIterator[StreamEvent]":
    return self._dream_impl(agent_id, now_ms)

  async def _dream_impl(self, agent_id: str, now_ms: int | None = None) -> "AsyncIterator[StreamEvent]":
    from deepstrike._kernel import IdlePipeline, MemoryEntry as KernelMemoryEntry, SessionData as KernelSessionData
    from deepstrike.memory.protocols import (
      CurationResult,
      CurationStats,
      DreamResult,
      MemoryEntry,
    )

    if self._opts.dream_store is None:
      raise RuntimeError("dream_store not configured")

    if now_ms is None:
      now_ms = int(time.time() * 1000)

    sessions = await self._opts.dream_store.load_sessions(agent_id)
    existing = await self._opts.dream_store.load_memories(agent_id)
    if not sessions:
      yield DoneEvent(iterations=0, total_tokens=0, status="completed", dream_result=DreamResult())
      return

    pipeline = IdlePipeline(agent_id)
    action1 = pipeline.feed_trigger(
      [
        KernelSessionData(
          session_id=s.session_id,
          agent_id=s.agent_id,
          messages=[_to_kernel_message(m) for m in s.messages],
          metadata=json.dumps(s.metadata) if s.metadata is not None else "null",
          created_at_ms=s.created_at_ms,
          updated_at_ms=s.updated_at_ms,
        )
        for s in sessions
      ],
      [
        KernelMemoryEntry(text=e.text, score=e.score, metadata=json.dumps(e.metadata) if e.metadata is not None else "null")
        for e in existing
      ],
      now_ms,
    )
    if action1.kind in ("noop", "aborted"):
      yield DoneEvent(iterations=0, total_tokens=0, status="completed", dream_result=DreamResult())
      return
    if action1.kind != "synthesize_insights":
      raise RuntimeError(f"unexpected idle action: {action1.kind}")

    synthesis_text = ""
    total_tokens = 0
    dream_provider = self._opts.dream_provider or self._opts.provider
    create_run_state = getattr(dream_provider, "create_run_state", None)
    provider_state = create_run_state() if callable(create_run_state) else None
    synth_msgs = list(action1.messages or [])
    kernel_system_text = "\n\n".join(m.content for m in synth_msgs if m.role == "system")
    synth_context = RenderedContext(
      system_text="\n\n".join(filter(None, [kernel_system_text, self._opts.dream_system_prompt])),
      turns=[m for m in synth_msgs if m.role != "system"],
    )
    async for evt in dream_provider.stream(synth_context, [], extensions=None, state=provider_state):
      if isinstance(evt, TextDelta):
        synthesis_text += evt.delta
        yield evt
      elif getattr(evt, "type", None) == "usage":
        total_tokens = getattr(evt, "total_tokens", 0)

    action2 = pipeline.feed_synthesis_result(synthesis_text)
    if action2.kind != "commit_memories":
      raise RuntimeError(f"unexpected idle action: {action2.kind}")

    cr = action2.curation_result
    rr = action2.run_result
    ds_result = CurationResult(
      to_add=[MemoryEntry(text=e.text, score=e.score, metadata=_parse_meta(e.metadata)) for e in (cr.to_add or [])],
      to_remove_indices=list(cr.to_remove_indices or []),
      stats=CurationStats(
        insights_processed=cr.stats.insights_processed if cr.stats else 0,
        duplicates_removed=cr.stats.duplicates_removed if cr.stats else 0,
        conflicts_resolved=cr.stats.conflicts_resolved if cr.stats else 0,
        entries_added=cr.stats.entries_added if cr.stats else 0,
      ),
    )
    await self._opts.dream_store.commit(agent_id, ds_result, existing)
    yield DoneEvent(
      iterations=1, total_tokens=total_tokens, status="completed",
      dream_result=DreamResult(
        sessions_processed=rr.sessions_processed if rr else 0,
        insights_extracted=rr.insights_extracted if rr else 0,
        entries_added=ds_result.stats.entries_added,
        entries_removed=len(ds_result.to_remove_indices),
      ),
    )

  async def _resolve_kernel_suspend(
    self,
    runtime: KernelRuntime,
    session_id: str,
  ) -> tuple[list[str], list[str], list[StreamEvent]]:
    from deepstrike.runtime.execution_plane import resolve_permission_request

    gated = [
      o for o in self._pending_observations
      if o.get("kind") == "tool_gated" and isinstance(o.get("call_id"), str) and isinstance(o.get("tool"), str)
    ]
    approved: list[str] = []
    denied: list[str] = []
    events: list[StreamEvent] = []
    run_ctx = RunContext(on_permission_request=self._opts.on_permission_request)

    for g in gated:
      request = PermissionRequestEvent(
        call_id=g["call_id"],
        tool_name=g["tool"],
        arguments="{}",
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
        "arguments": "{}",
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

  async def _execute(
    self,
    session_id: str,
    goal: str,
    criteria: list[str],
    extensions: dict | None,
    prior_events: list[SessionEntry] | None,
    resume_mid_run: bool,
  ) -> AsyncIterator[StreamEvent]:
    self._interrupted = False
    self._pending_observations = []
    self._pending_spool_outputs.clear()
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

    policy = LoopPolicy(
      max_tokens=self._opts.max_tokens,
      max_turns=effective_max_turns,
      timeout_ms=effective_timeout_ms,
    )
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
          "kind": "add_memory_message",
          "content": mem,
          "tokens": max(1, len(mem) // 4),
        })

    skill_dir = Path(self._opts.skill_dir) if self._opts.skill_dir else None
    if skill_dir and skill_dir.is_dir():
      from deepstrike.skills.registry import SkillRegistry
      registry = SkillRegistry(str(skill_dir))
      skills = [
        SkillMetadata(
          name=m.name,
          description=m.description or "",
          when_to_use=getattr(m, "when_to_use", None),
          effort=getattr(m, "effort", None),
          estimated_tokens=getattr(m, "estimated_tokens", 0) or 0,
        )
        for m in registry.scan()
      ]
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_available_skills",
        "skills": [skill_metadata_to_kernel(skill) for skill in skills],
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

    session_start = int(time.time() * 1000)
    start_payload = {
      "kind": "start_run",
      "task": {"goal": goal, "criteria": criteria},
    }
    if self._opts.run_spec:
      from deepstrike.types.agent import agent_run_spec_to_kernel
      start_payload["run_spec"] = agent_run_spec_to_kernel(self._opts.run_spec)

    os_profile = assert_native_profile(self._opts.os_profile or "native")
    gov_policy = self._opts.governance_policy or os_profile.governance_policy
    kernel_apply(
      runtime,
      self._pending_observations,
      governance_policy_to_kernel_event(gov_policy),
    )

    ap = self._opts.attention_policy or os_profile.attention_policy
    max_q = ap.get("max_queue_size") if isinstance(ap, dict) else getattr(ap, "max_queue_size", None)
    kernel_apply(runtime, self._pending_observations, {
      "kind": "set_attention_policy",
      **({"max_queue_size": max_q} if max_q is not None else {}),
    })

    scheduler_budget = _scheduler_budget_to_kernel(self._opts.scheduler_budget)
    if scheduler_budget is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_scheduler_budget",
        **scheduler_budget,
      })

    if self._opts.resource_quota is not None:
      kernel_apply(runtime, self._pending_observations, {
        "kind": "set_resource_quota",
        "quota": _resource_quota_to_kernel(self._opts.resource_quota),
      })

    action = (
      kernel_action(runtime, self._pending_observations, {"kind": "resume"})
      if resume_mid_run
      else kernel_action(runtime, self._pending_observations, start_payload)
    )
    has_attempted_reactive_compact = False

    while not runtime.is_terminal():
      if action.kind == "execute_tool":
        await self._apply_kernel_page_in(runtime, session_id)
      next_compressed_archive_start = await self._append_observations(
        session_id, runtime, next_compressed_archive_start,
      )
      self._next_archive_start = next_compressed_archive_start
      if self._interrupted:
        action = kernel_action(runtime, self._pending_observations, {"kind": "timeout"})
        break

      if self._opts.signal_source:
        sig = await self._opts.signal_source.next_signal()
        if sig:
          sig_action = kernel_maybe_action(runtime, self._pending_observations, {
            "kind": "signal",
            "signal": {
              "id": str(uuid.uuid4()),
              "source": sig.source,
              "signal_type": sig.signal_type,
              "urgency": sig.urgency,
              "summary": str(sig.payload.get("goal") or sig.kind),
              "payload": sig.payload,
              **({"dedupe_key": sig.dedupe_key} if sig.dedupe_key else {}),
              "timestamp_ms": int(time.time() * 1000),
            },
          })
          if sig_action:
            action = sig_action
      if runtime.is_terminal():
        break

      if action.kind == "call_provider":
        final_tool_calls: list[ToolCall] = []
        final_text = ""
        context = action.context or RenderedContext()
        turn_tokens = 0
        should_retry = False
        try:
          async for evt in self._opts.provider.stream(
            context, action.tools or [], extensions=ext if ext else None, state=provider_state,
          ):
            if getattr(evt, "type", None) == "usage":
              turn_tokens = getattr(evt, "total_tokens", 0)
              continue
            yield evt
            if isinstance(evt, TextDelta):
              final_text += evt.delta
            elif isinstance(evt, ToolCallEvent):
              final_tool_calls.append(ToolCall(
                id=evt.id, name=evt.name, arguments=json.dumps(evt.arguments),
              ))
        except Exception as exc:
          err_msg = str(exc).lower()
          if (
            ("413" in err_msg or "too long" in err_msg or "context length exceeded" in err_msg or "context_length_exceeded" in err_msg)
            and not has_attempted_reactive_compact
          ):
            has_attempted_reactive_compact = True
            if force_compact(runtime, self._pending_observations):
              next_compressed_archive_start = await self._append_observations(
                session_id, runtime, next_compressed_archive_start,
              )
              should_retry = True
          
          if not should_retry:
            yield ErrorEvent(message=str(exc))
            action = kernel_action(runtime, self._pending_observations, {"kind": "timeout"})
            break

        if should_retry:
          action = SimpleNamespace(
            kind="call_provider",
            context=runtime.render(),
            tools=action.tools or [],
          )
          continue

        assistant_message = Message(
          role="assistant", content=final_text, tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
        )
        provider_event: dict[str, Any] = {
          "kind": "provider_result",
          "message": message_to_kernel(assistant_message),
          "now_ms": int(time.time() * 1000),
        }
        next_action = kernel_maybe_action(runtime, self._pending_observations, provider_event)
        if not next_action and any(o.get("kind") == "suspended" for o in self._pending_observations):
          approved, denied, suspend_events = await self._resolve_kernel_suspend(runtime, session_id)
          for evt in suspend_events:
            yield evt
          next_action = kernel_action(runtime, self._pending_observations, {
            "kind": "resume",
            "approved_calls": approved,
            "denied_calls": denied,
          })
        action = next_action or kernel_action(runtime, self._pending_observations, provider_event)
        from deepstrike.runtime.provider_replay import peek_provider_replay
        provider_replay = peek_provider_replay(self._opts.provider, final_text, final_tool_calls)
        await self._opts.session_log.append(session_id, build_llm_completed_event(
          turn=runtime.turn(),
          content=final_text,
          tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
          provider_replay=provider_replay,
        ))

      elif action.kind == "execute_tool":
        all_calls = list(action.calls or [])
        await self._opts.session_log.append(session_id, {
          "kind": "tool_requested", "turn": runtime.turn(), "calls": all_calls,
        })
        from deepstrike.runtime.large_result_spool import LargeResultSpool
        run_ctx = RunContext(
          agent_id=self._opts.agent_id,
          skill_dir=skill_dir,
          dream_store=self._opts.dream_store,
          knowledge_source=self._opts.knowledge_source,
          on_tool_suspend=self._opts.on_tool_suspend,
          on_permission_request=self._opts.on_permission_request,
          result_spool=self._opts.result_spool or LargeResultSpool(),
        )
        tool_results: list[ToolResult] = []
        normal_calls = [c for c in all_calls if c.name != "update_plan"]
        plan_calls = [c for c in all_calls if c.name == "update_plan"]

        for call in plan_calls:
          update = _parse_update_plan_args(call.arguments)
          kernel_apply(runtime, self._pending_observations, {
            "kind": "update_task",
            "update": task_update_to_kernel(update),
          })
          result = ToolResult(call_id=call.id, output="success", is_error=False)
          tool_results.append(result)
          yield ToolResultEvent(call_id=call.id, content="success", is_error=False)

        if normal_calls:
          async for evt in self._plane.execute_all(normal_calls, run_ctx):
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
          names = ", ".join(c.name for c in normal_calls)
          kernel_apply(runtime, self._pending_observations, {
            "kind": "update_task",
            "update": task_update_to_kernel(TaskUpdate(progress=f"Executed tools: {names}")),
          })

        await self._opts.session_log.append(session_id, {
          "kind": "tool_completed", "turn": runtime.turn(), "results": tool_results,
        })
        for call in normal_calls:
          result = next((r for r in tool_results if r.call_id == call.id), None)
          if result is not None:
            self._pending_spool_outputs[call.id] = {"tool": call.name, "output": result.output}
        action = kernel_action(runtime, self._pending_observations, {
          "kind": "tool_results",
          "results": [tool_result_to_kernel(result) for result in tool_results],
        })

      elif action.kind == "evaluate_milestone":
        milestone_policy = self._opts.milestone_policy or "require_verifier"
        if milestone_policy == "auto_pass":
          from deepstrike.types.agent import milestone_check_result_to_kernel, milestone_check_pass
          action = kernel_action(runtime, self._pending_observations, {
            "kind": "milestone_result",
            "result": milestone_check_result_to_kernel(milestone_check_pass(action.phase_id)),
          })
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start,
          )
        elif self._opts.on_milestone_evaluate is not None:
          import inspect
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
            "result": milestone_check_result_to_kernel(check),
          })
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start,
          )
        else:
          next_compressed_archive_start = await self._append_observations(
            session_id, runtime, next_compressed_archive_start,
          )
          turns_used = max(1, runtime.turn())
          await self._opts.session_log.append(session_id, build_run_terminal_event(
            reason="milestone_pending",
            turns_used=turns_used,
            total_tokens=0,
          ))
          self._active_kernel = None
          self._current_session_id = None
          yield DoneEvent(iterations=turns_used, total_tokens=0, status="milestone_pending")
          return

      elif action.kind == "done":
        break

    result = action.result if action.kind == "done" else None
    status = result.termination if result else "error"
    turns_used = max(1, result.turns_used) if result else (runtime.turn() or 0)
    total_tokens = result.total_tokens_used if result else 0

    next_compressed_archive_start = await self._append_observations(
      session_id, runtime, next_compressed_archive_start,
    )
    await self._opts.session_log.append(session_id, build_run_terminal_event(
      reason=status,
      turns_used=turns_used,
      total_tokens=total_tokens,
    ))

    if self._opts.dream_store and self._opts.agent_id:
      new_msgs = list(runtime.drain_new_messages())
      if new_msgs:
        try:
          from deepstrike.memory.protocols import SessionData
          now_ms = int(time.time() * 1000)
          await self._opts.dream_store.save_session(SessionData(
            session_id=str(uuid.uuid4()),
            agent_id=self._opts.agent_id,
            messages=new_msgs,
            created_at_ms=session_start,
            updated_at_ms=now_ms,
          ))
        except Exception:
          pass

    self._active_kernel = None
    self._current_session_id = None
    yield DoneEvent(iterations=turns_used, total_tokens=total_tokens, status=status)

  async def _apply_kernel_page_in(self, runtime: KernelRuntime, session_id: str) -> None:
    """Phase 4: satisfy kernel page-in requests before meta-tool execution."""
    requests = [
      o for o in self._pending_observations
      if o.get("kind") == "page_in_requested" and isinstance(o.get("tool"), str)
    ]
    if not requests:
      return
    entries: list[dict[str, Any]] = []
    for req in requests:
      query = req.get("query") if isinstance(req.get("query"), str) else ""
      top_k = req.get("top_k") if isinstance(req.get("top_k"), int) else 5
      tool = req.get("tool")
      if tool == "memory":
        local_hits = []
        for m in self._local_page_out_cache:
          content = m.get("content") if isinstance(m, dict) else getattr(m, "content", None)
          if isinstance(content, str) and query.lower() in content.lower():
            local_hits.append(m)
        local_hits = local_hits[:top_k]

        for hit in local_hits:
          if isinstance(hit, dict):
            role = hit.get("role") or "system"
            content = hit.get("content") or ""
          else:
            role = getattr(hit, "role", "system") or "system"
            content = getattr(hit, "content", "") or ""
          entries.append({
            "content": f"[local semantic cache] {role}: {content}",
            "source": "semantic_cache",
          })

        remaining_k = top_k - len(entries)
        if remaining_k > 0 and self._opts.dream_store and self._opts.agent_id:
          hits = await self._opts.dream_store.search(self._opts.agent_id, query, remaining_k)
          for hit in hits:
            entries.append({
              "content": f"[memory score={hit.score:.3f}] {hit.text}",
              "source": "memory",
            })
      elif tool == "knowledge" and self._opts.knowledge_source:
        snippets = await self._opts.knowledge_source.retrieve(query, top_k)
        for snippet in snippets:
          entries.append({"content": snippet, "source": "knowledge"})
    if not entries:
      return
    kernel_apply(runtime, self._pending_observations, {"kind": "page_in", "entries": entries})
    await self._opts.session_log.append(session_id, with_category({
      "kind": "page_in",
      "turn": runtime.turn(),
      "entry_count": len(entries),
    }))

  async def _append_observations(
    self,
    session_id: str,
    runtime: KernelRuntime,
    next_archive_start: int,
  ) -> int:
    turn = runtime.turn()
    preserved_refs = runtime.preserved_refs()
    observations = self._pending_observations
    self._pending_observations = []
    for obs in observations:
      if obs.get("kind") == "page_in_requested":
        continue

      archive_ref = None
      spool_ref = None
      if obs.get("kind") == "compressed":
        archived = obs.get("archived")
        if self._opts.compression_store and archived:
          try:
            path_ref = await self._opts.compression_store.write(session_id, next_archive_start, archived)
            if path_ref:
              archive_ref = path_ref
          except Exception:
            pass

      if obs.get("kind") == "page_out" and obs.get("archived"):
        self._local_page_out_cache.extend(obs["archived"])

      if obs.get("kind") == "large_result_spooled":
        call_id = obs.get("call_id") if isinstance(obs.get("call_id"), str) else ""
        pending = self._pending_spool_outputs.pop(call_id, None)
        if pending:
          try:
            from deepstrike.runtime.large_result_spool import LargeResultSpool
            spool = self._opts.result_spool or LargeResultSpool()
            spool_ref = await spool.persist_output(call_id, pending["output"])
          except Exception:
            pass
          if not obs.get("tool") and pending.get("tool"):
            obs = {**obs, "tool": pending["tool"]}

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
        archive_ref=archive_ref,
        preserved_refs=preserved_refs,
        compression_action=_compression_action,
        spool_ref=spool_ref,
      )
      if not event:
        continue
      compressed_seq = await self._opts.session_log.append(session_id, event)
      if event.get("kind") == "compressed":
        next_archive_start = compressed_seq + 1
      if (
        obs.get("kind") == "page_out"
        and obs.get("tier_hint") == "semantic"
        and isinstance(obs.get("archived"), list)
        and obs["archived"]
      ):
        import asyncio
        asyncio.create_task(self._archive_semantic_page_out(list(obs["archived"]), _compression_action(obs.get("action"))))
    return next_archive_start

  async def _archive_semantic_page_out(self, archived: list[Any], action: str | None = None) -> None:
    if not self._opts.dream_store or not self._opts.agent_id:
      return
    try:
      if self._opts.dream_summarizer:
        import inspect
        result = self._opts.dream_summarizer(archived, {"action": action})
        summary = await result if inspect.isawaitable(result) else result
      else:
        summary = await self._summarize_for_long_term_memory(archived)
      existing = await self._opts.dream_store.load_memories(self._opts.agent_id)
      from deepstrike.memory.protocols import CurationResult, CurationStats, MemoryEntry
      await self._opts.dream_store.commit(
        self._opts.agent_id,
        CurationResult(
          to_add=[MemoryEntry(text=summary, score=1.0, metadata={"source": "semantic_page_out", "action": action})],
          to_remove_indices=[],
          stats=CurationStats(insights_processed=1, entries_added=1),
        ),
        existing,
      )
    except Exception:
      pass

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


def _compression_action(action: str | None) -> str | None:
  if action in ("snip_compact", "micro_compact", "context_collapse", "auto_compact"):
    return action
  return None


def _is_mid_run(events: list[SessionEntry]) -> bool:
  return bool(events) and not any(e.event.get("kind") == "run_terminal" for e in events)


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
  return messages


async def _replay_messages_async(
  events: list[SessionEntry],
  max_bytes: int | None = None,
  load_archive: Callable[[str], Awaitable[list[Message]]] | None = None,
) -> list[Message]:
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
      loaded_successfully = False
      archive_ref = e.get("archive_ref")
      if archive_ref and load_archive:
        try:
          archived_msgs = await load_archive(archive_ref)
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
  return messages


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
    max_depth = quota.get("max_spawn_depth")
    rate = quota.get("memory_writes_per_window")
  else:
    max_concurrent = quota.max_concurrent_subagents
    max_depth = quota.max_spawn_depth
    rate = quota.memory_writes_per_window

  out: dict[str, Any] = {}
  if max_concurrent is not None:
    out["max_concurrent_subagents"] = max_concurrent
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


def _scheduler_budget_to_kernel(budget: SchedulerBudget | dict[str, Any] | None) -> dict[str, Any] | None:
  if budget is None:
    return None
  if isinstance(budget, dict):
    max_wall_ms = budget.get("max_wall_ms", budget.get("maxWallMs"))
  else:
    max_wall_ms = budget.max_wall_ms
  return {"max_wall_ms": max_wall_ms} if max_wall_ms is not None else {}


def _to_kernel_message(message: object) -> Message:
  if isinstance(message, Message):
    return message
  role = getattr(message, "role", "user")
  content = getattr(message, "content", "")
  token_count = getattr(message, "token_count", None)
  tool_calls = getattr(message, "tool_calls", None) or []
  return Message(role=role, content=content, token_count=token_count, tool_calls=tool_calls)


def _parse_meta(raw: object) -> object | None:
  if raw is None:
    return None
  if isinstance(raw, str):
    try:
      return json.loads(raw)
    except Exception:
      return raw
  return raw


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
