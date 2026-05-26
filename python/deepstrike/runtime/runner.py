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
  LoopPolicy,
  LoopStateMachine,
  Message,
  RuntimeTask,
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
  ToolResultEvent,
  ToolSuspendEvent,
)
from deepstrike.runtime.execution_plane import ExecutionPlane, LocalExecutionPlane, RunContext
from deepstrike.runtime.replay_sanitize import sanitize_replay_text
from deepstrike.runtime.session_repair import (
  build_llm_completed_event,
  build_run_terminal_event,
  repair_events_for_recovery,
)
from deepstrike.runtime.session_log import SessionEntry, SessionEvent, SessionLog
from deepstrike.runtime.archive import ArchiveStore

if TYPE_CHECKING:
  from deepstrike.governance import Governance
  from deepstrike.knowledge.source import KnowledgeSource
  from deepstrike.memory.protocols import DreamResult, DreamStore
  from deepstrike.signals.types import SignalSource


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
  tokenizer: str | None = None
  enable_plan_tool: bool | None = None
  on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None


class RuntimeRunner:
  def __init__(self, opts: RuntimeOptions) -> None:
    self._opts = opts
    self._interrupted = False
    self._plane = opts.execution_plane or LocalExecutionPlane()

  def interrupt(self) -> None:
    self._interrupted = True

  @property
  def execution_plane(self) -> ExecutionPlane:
    return self._plane

  async def run_streaming(
    self,
    goal: str,
    *,
    criteria: list[str] | None = None,
    extensions: dict | None = None,
    session_id: str | None = None,
  ) -> AsyncIterator[StreamEvent]:
    """Streaming convenience entry; allocates a session id when omitted."""
    sid = session_id or str(uuid.uuid4())
    async for evt in self.run(
      session_id=sid,
      goal=goal,
      criteria=criteria,
      extensions=extensions,
    ):
      yield evt

  async def run(
    self,
    *,
    session_id: str,
    goal: str,
    criteria: list[str] | None = None,
    extensions: dict | None = None,
  ) -> AsyncIterator[StreamEvent]:
    prior = await self._opts.session_log.read(session_id)
    mid_run = _is_mid_run(prior)
    if not mid_run:
      await self._opts.session_log.append(session_id, {
        "kind": "run_started",
        "run_id": str(uuid.uuid4()),
        "goal": goal,
        "criteria": criteria or [],
        **({"agent_id": self._opts.agent_id} if self._opts.agent_id else {}),
        **({"system_prompt": self._opts.system_prompt} if self._opts.system_prompt else {}),
      })
    async for evt in self._execute(
      session_id, goal, criteria or [], extensions,
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

  async def dream(self, agent_id: str, now_ms: int | None = None) -> "DreamResult":
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
      return DreamResult()

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
      return DreamResult()
    if action1.kind != "synthesize_insights":
      raise RuntimeError(f"unexpected idle action: {action1.kind}")

    synthesis_text = ""
    create_run_state = getattr(self._opts.provider, "create_run_state", None)
    provider_state = create_run_state() if callable(create_run_state) else None
    synth_msgs = list(action1.messages or [])
    synth_context = RenderedContext(
      system_text="\n\n".join(m.content for m in synth_msgs if m.role == "system"),
      turns=[m for m in synth_msgs if m.role != "system"],
    )
    async for evt in self._opts.provider.stream(synth_context, [], extensions=None, state=provider_state):
      if isinstance(evt, TextDelta):
        synthesis_text += evt.delta

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
    return DreamResult(
      sessions_processed=rr.sessions_processed if rr else 0,
      insights_extracted=rr.insights_extracted if rr else 0,
      entries_added=ds_result.stats.entries_added,
      entries_removed=len(ds_result.to_remove_indices),
    )

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
    ext = {**(self._opts.extensions or {}), **(extensions or {})}
    create_run_state = getattr(self._opts.provider, "create_run_state", None)
    provider_state = create_run_state() if callable(create_run_state) else None
    next_compressed_archive_start = _next_archived_seq_start(prior_events)

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
    sm = LoopStateMachine(policy)
    router = SignalRouter(max_queue_size=256)

    if self._opts.tokenizer:
      sm.set_tokenizer(self._opts.tokenizer)
    if self._opts.enable_plan_tool is not None:
      sm.set_plan_tool_enabled(self._opts.enable_plan_tool)

    sm.set_tools(self._plane.schemas())

    if self._opts.system_prompt:
      sm.add_system_message(self._opts.system_prompt, max(1, len(self._opts.system_prompt) // 4))

    if self._opts.initial_memory:
      for mem in self._opts.initial_memory:
        sm.add_memory_message(mem, max(1, len(mem) // 4))

    skill_dir = Path(self._opts.skill_dir) if self._opts.skill_dir else None
    if skill_dir and skill_dir.is_dir():
      from deepstrike.skills.registry import SkillRegistry
      registry = SkillRegistry(str(skill_dir))
      sm.set_available_skills([
        SkillMetadata(
          name=m.name,
          description=m.description or "",
          when_to_use=getattr(m, "when_to_use", None),
          effort=getattr(m, "effort", None),
          estimated_tokens=getattr(m, "estimated_tokens", 0) or 0,
        )
        for m in registry.scan()
      ])

    if self._opts.dream_store and self._opts.agent_id:
      sm.set_memory_enabled(True)
    if self._opts.knowledge_source:
      sm.set_knowledge_enabled(True)

    max_bytes = sm.recovery_content_bytes()

    if prior_events:
      from deepstrike.runtime.provider_replay import seed_provider_replay_from_events
      repaired = repair_events_for_recovery(prior_events, max_bytes)
      seed_provider_replay_from_events(self._opts.provider, repaired)
      sm.preload_history(_replay_messages(repaired, max_bytes))

    session_start = int(time.time() * 1000)
    action = sm.resume_after_preload() if resume_mid_run else sm.start(RuntimeTask(goal, criteria=criteria))
    has_attempted_reactive_compact = False

    while not sm.is_terminal():
      next_compressed_archive_start = await self._append_observations(
        session_id, sm, next_compressed_archive_start,
      )
      if self._interrupted:
        action = sm.feed_timeout()
        break

      if self._opts.signal_source:
        sig = await self._opts.signal_source.next_signal()
        if sig:
          disposition = router.ingest(sig.to_kernel_signal(), action.kind == "execute_tools")
          if disposition == "interrupt_now":
            action = sm.feed_timeout()
            break

      queued = router.next()
      while queued:
        if queued.urgency == "critical":
          action = sm.feed_timeout()
          break
        queued = router.next()
      if sm.is_terminal():
        break

      if action.kind == "call_llm":
        final_tool_calls: list[ToolCall] = []
        final_text = ""
        raw_ctx = action.context
        context = RenderedContext(
          system_text=getattr(raw_ctx, "system_text", "") or "",
          turns=list(getattr(raw_ctx, "turns", [])),
        )
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
            compacted = sm.force_compact()
            if compacted:
              next_compressed_archive_start = await self._append_observations(
                session_id, sm, next_compressed_archive_start,
              )
              should_retry = True
          
          if not should_retry:
            yield ErrorEvent(message=str(exc))
            action = sm.feed_timeout()
            break

        if should_retry:
          action = SimpleNamespace(
            kind="call_llm",
            context=sm.render(),
            tools=action.tools or [],
          )
          continue

        action = sm.feed_llm_response(Message(
          role="assistant", content=final_text, tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
        ))
        from deepstrike.runtime.provider_replay import peek_provider_replay
        provider_replay = peek_provider_replay(self._opts.provider, final_text, final_tool_calls)
        await self._opts.session_log.append(session_id, build_llm_completed_event(
          turn=sm.turn(),
          content=final_text,
          tool_calls=final_tool_calls,
          token_count=turn_tokens or None,
          provider_replay=provider_replay,
        ))

      elif action.kind == "execute_tools":
        all_calls = list(action.calls or [])
        await self._opts.session_log.append(session_id, {
          "kind": "tool_requested", "turn": sm.turn(), "calls": all_calls,
        })
        run_ctx = RunContext(
          agent_id=self._opts.agent_id,
          skill_dir=skill_dir,
          dream_store=self._opts.dream_store,
          knowledge_source=self._opts.knowledge_source,
          governance=self._opts.governance,
          on_tool_suspend=self._opts.on_tool_suspend,
        )
        tool_results: list[ToolResult] = []
        normal_calls = [c for c in all_calls if c.name != "update_plan"]
        plan_calls = [c for c in all_calls if c.name == "update_plan"]

        for call in plan_calls:
          update = _parse_update_plan_args(call.arguments)
          sm.update_task(update)
          result = ToolResult(call_id=call.id, output="success", is_error=False)
          tool_results.append(result)
          yield ToolResultEvent(call_id=call.id, content="success", is_error=False)

        if normal_calls:
          async for evt in self._plane.execute_all(normal_calls, run_ctx):
            yield evt
            if isinstance(evt, ToolResultEvent):
              tool_results.append(ToolResult(
                call_id=evt.call_id, output=evt.content, is_error=evt.is_error,
              ))
          names = ", ".join(c.name for c in normal_calls)
          sm.update_task(TaskUpdate(progress=f"Executed tools: {names}"))

        await self._opts.session_log.append(session_id, {
          "kind": "tool_completed", "turn": sm.turn(), "results": tool_results,
        })
        action = sm.feed_tool_results(tool_results)

      elif action.kind == "done":
        break

    result = action.result
    status = result.termination if result else "error"
    turns_used = max(1, result.turns_used) if result else 0
    total_tokens = result.total_tokens_used if result else 0

    next_compressed_archive_start = await self._append_observations(
      session_id, sm, next_compressed_archive_start,
    )
    await self._opts.session_log.append(session_id, build_run_terminal_event(
      reason=status,
      turns_used=turns_used,
      total_tokens=total_tokens,
    ))

    if self._opts.dream_store and self._opts.agent_id:
      new_msgs = list(sm.drain_new_messages())
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

    yield DoneEvent(iterations=turns_used, total_tokens=total_tokens, status=status)

  async def _append_observations(
    self,
    session_id: str,
    sm: LoopStateMachine,
    next_archive_start: int,
  ) -> int:
    for obs in sm.take_observations():
      if getattr(obs, "kind", None) != "compressed":
        continue
      latest = await self._opts.session_log.latest_seq(session_id)
      if latest < next_archive_start:
        continue
      end = latest

      archive_ref = None
      archived = getattr(obs, "archived", None)
      if self._opts.compression_store and archived:
        try:
          path_ref = await self._opts.compression_store.write(session_id, next_archive_start, archived)
          if path_ref:
            archive_ref = path_ref
        except Exception:
          pass

      summary = getattr(obs, "summary", None)
      summary_tokens = max(1, len(summary) // 4) if summary else None

      compressed_seq = await self._opts.session_log.append(session_id, {
        "kind": "compressed",
        "turn": sm.turn(),
        "archived_seq_range": (next_archive_start, end),
        "action": getattr(obs, "action", "none"),
        "summary": summary or "",
        "summary_tokens": summary_tokens,
        "archive_ref": archive_ref,
        "preserved_refs": sm.preserved_refs(),
      })
      next_archive_start = compressed_seq + 1
    return next_archive_start


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
  return messages


def _next_archived_seq_start(events: list[SessionEntry] | None) -> int:
  next_seq = 0
  for entry in events or []:
    event = entry.event
    if event.get("kind") == "compressed":
      next_seq = max(next_seq, int(event["archived_seq_range"][1]) + 1)
  return next_seq


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
