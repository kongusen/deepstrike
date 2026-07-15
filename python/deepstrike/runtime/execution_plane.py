from __future__ import annotations

import asyncio
import inspect
import json
import warnings
from collections.abc import AsyncIterable, AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from deepstrike._kernel import ToolCall, ToolResult, ToolSchema
from deepstrike.providers.stream import (
  PermissionRequestEvent,
  PermissionResolvedEvent,
  PermissionResponse,
  StreamEvent,
  ToolAuditFailedEvent,
  ToolDeltaEvent,
  ToolDeniedEvent,
  ToolResultEvent,
  ToolSuspendEvent,
  ToolArgumentRepairedEvent,
)
from deepstrike.tools.errors import format_tool_error
from deepstrike.tools.registry import RegisteredTool, normalize_tool_chunk, tool_chunk_text, validate_tool_arguments
from deepstrike.skills.loader import read_skill_file

if TYPE_CHECKING:
  from deepstrike.governance import Governance
  from deepstrike.knowledge.source import KnowledgeSource
  from deepstrike.memory.protocols import DreamStore, MemoryScope
  from deepstrike.runtime.large_result_spool import LargeResultSpool
  from deepstrike.runtime.reliability import OperationContext


def _strip_frontmatter(content: str) -> str:
  import re
  return re.sub(r"^---\n.*?\n---\n?", "", content, count=1, flags=re.DOTALL)


_WARNED_FAILURE_SHAPES: set[str] = set()


def _maybe_warn_failure_shaped_chunk(tool_name: str, delta_text: str) -> None:
  """One-shot heuristic: detect when a streaming tool yielded text that *looks* like a failure
  envelope. The runtime cannot block the tool from doing it, but we warn (once per tool) so
  the author migrates to raising — the canonical "streaming tool fails" path. Aligns with the
  non-streaming ``tool()`` / ``safe_tool`` contract: failures raise, successes return data."""
  if not delta_text or tool_name in _WARNED_FAILURE_SHAPES:
    return
  trimmed = delta_text.strip()
  if len(trimmed) < 2 or trimmed[0] != "{":
    return
  try:
    parsed = json.loads(trimmed)
  except Exception:
    return
  if not isinstance(parsed, dict):
    return
  looks_like_failure = parsed.get("success") is False or parsed.get("isError") is True or parsed.get("is_error") is True
  if not looks_like_failure:
    return
  _WARNED_FAILURE_SHAPES.add(tool_name)
  warnings.warn(
    f'streaming tool "{tool_name}" yielded a failure-shaped chunk '
    "(success:false / isError:true). Streaming tools should fail by raising; the runtime will "
    "catch and surface the error consistently. Returning a failure-shaped chunk is a foot-gun: "
    "the kernel still sees is_error=False.",
    RuntimeWarning,
    stacklevel=2,
  )


@dataclass
class RunContext:
  operation: "OperationContext | None" = None
  agent_id: str | None = None
  memory_scope: "MemoryScope | None" = None
  skill_dir: Path | None = None
  dream_store: "DreamStore | None" = None
  knowledge_source: "KnowledgeSource | None" = None
  on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None
  on_permission_request: Callable[[PermissionRequestEvent], Awaitable[PermissionResponse | bool | dict[str, Any]] | PermissionResponse | bool | dict[str, Any]] | None = None
  result_spool: "LargeResultSpool | None" = None
  # M3/G4: the working directory a sub-agent's tools should run in (the git worktree created for an
  # ``isolation: "worktree"`` node). Injected by ``WorktreeExecutionPlane``; a cwd-aware tool reads it.
  cwd: str | None = None
  # Per-call best-effort side-effect helper (injected by the execution plane). Wrap audit-log
  # writes, metrics emits, or any non-essential persistence in ``await ctx.audit(label, fn)``;
  # if ``fn`` raises, the failure is surfaced as a ``ToolAuditFailedEvent`` and the tool still
  # completes successfully — avoiding the foot-gun where a transient audit-store outage flips
  # an already-committed write into ``is_error=True`` and triggers a duplicate retry.
  audit: Callable[[str, Callable[[], Awaitable[None] | None]], Awaitable[None]] | None = None


class ExecutionPlane:
  def register(self, *tools: RegisteredTool) -> "ExecutionPlane": ...
  def unregister(self, name: str) -> "ExecutionPlane": ...
  def schemas(self) -> list[ToolSchema]: ...
  def execute_all(self, calls: list[ToolCall], ctx: RunContext) -> AsyncIterator[StreamEvent]: ...


class LocalExecutionPlane:
  def __init__(self) -> None:
    self._tools: dict[str, RegisteredTool] = {}

  def register(self, *tools: RegisteredTool) -> "LocalExecutionPlane":
    for t in tools:
      self._tools[t.schema.name] = t
    return self

  def unregister(self, name: str) -> "LocalExecutionPlane":
    self._tools.pop(name, None)
    return self

  def schemas(self) -> list[ToolSchema]:
    return [t.schema for t in self._tools.values()]

  async def execute_all(self, calls: list[ToolCall], ctx: RunContext) -> AsyncIterator[StreamEvent]:
    permitted = calls

    skill_calls = [c for c in permitted if c.name == "skill"]
    memory_calls = [c for c in permitted if c.name == "memory"]
    knowledge_calls = [c for c in permitted if c.name == "knowledge"]
    regular_calls = [c for c in permitted if c.name not in ("skill", "memory", "knowledge")]

    for c in skill_calls:
      args = _parse_json(c.arguments)
      name = str(args.get("name", ""))
      content = None
      if ctx.skill_dir and name:
        content = read_skill_file(ctx.skill_dir, name)
      output = content if content is not None else f'Skill "{name}" not found.'
      yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=content is None)

    for c in memory_calls:
      if ctx.dream_store and ctx.agent_id and ctx.memory_scope:
        from deepstrike.memory.protocols import MemoryQuery
        args = _parse_json(c.arguments)
        query = str(args.get("query", ""))
        top_k = int(args.get("top_k", 5))
        entries = await ctx.dream_store.search(ctx.agent_id, MemoryQuery(
          scope=ctx.memory_scope, query=query, top_k=top_k,
        ))
        output = (
          "\n---\n".join(f"[memory record_id={e.record.record_id} score={e.score:.3f}] {e.record.content}" for e in entries)
          if entries
          else "No relevant memories found."
        )
        is_error = False
      else:
        output = "Memory retrieval not configured."
        is_error = True
      yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=is_error)

    for c in knowledge_calls:
      if ctx.knowledge_source:
        args = _parse_json(c.arguments)
        query = str(args.get("query", ""))
        top_k = int(args.get("top_k", 5))
        snippets = await ctx.knowledge_source.retrieve(query, top_k)
        output = "\n---\n".join(snippets) if snippets else "No relevant knowledge found."
        is_error = False
      else:
        output = "Knowledge source not configured."
        is_error = True
      yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=is_error)

    if regular_calls:
      queue: asyncio.Queue[tuple[str, object]] = asyncio.Queue()

      async def run_one(call: ToolCall) -> None:
        async for evt in self._execute_single(call, ctx):
          if isinstance(evt, ToolResultEvent):
            await queue.put(("result", evt))
          else:
            await queue.put(("stream", evt))

      tasks = [asyncio.create_task(run_one(c)) for c in regular_calls]
      pending = len(tasks)
      while pending:
        kind, item = await queue.get()
        if kind == "stream":
          yield item  # type: ignore[misc]
        else:
          yield item
          pending -= 1
      await asyncio.gather(*tasks)

  async def _try_read_spooled_argument(self, call: ToolCall, ctx: RunContext) -> str | None:
    is_read_tool = call.name in ("read", "read_file", "view_file", "read_spooled_result")
    if not is_read_tool:
      return None

    try:
      args = json.loads(call.arguments or "{}")
      for val in args.values():
        if isinstance(val, str) and (val.startswith(".spool/") or "/.spool/" in val):
          from deepstrike.runtime.large_result_spool import LargeResultSpool
          spool = ctx.result_spool or LargeResultSpool()
          content = await spool.read_spooled_result(val)
          return content
    except Exception:
      pass
    return None

  async def _execute_single(self, call: ToolCall, ctx: RunContext) -> AsyncIterator[StreamEvent]:
    spooled_content = await self._try_read_spooled_argument(call, ctx)
    if spooled_content is not None:
      yield ToolResultEvent(call_id=call.id, name=call.name, content=spooled_content, is_error=False)
      return

    registered = self._tools.get(call.name)
    if registered is None:
      yield ToolResultEvent(
        call_id=call.id,
        name=call.name,
        content=f"unknown tool: {call.name}",
        is_error=True,
        is_fatal=False,
        error_kind="recoverable",
      )
      return
    # Per-call ``audit`` helper: failures collected here are surfaced as
    # ``ToolAuditFailedEvent`` rather than flipping the main tool result to ``is_error=True``.
    audit_failures: list[tuple[str, str]] = []

    async def _audit(label: str, fn: Callable[[], Awaitable[None] | None]) -> None:
      try:
        result = fn()
        if inspect.isawaitable(result):
          await result
      except Exception as ae:
        audit_failures.append((label, format_tool_error(ae)))

    call_ctx = RunContext(
      operation=ctx.operation,
      agent_id=ctx.agent_id,
      memory_scope=ctx.memory_scope,
      skill_dir=ctx.skill_dir,
      dream_store=ctx.dream_store,
      knowledge_source=ctx.knowledge_source,
      on_tool_suspend=ctx.on_tool_suspend,
      on_permission_request=ctx.on_permission_request,
      result_spool=ctx.result_spool,
      cwd=ctx.cwd,
      audit=_audit,
    )
    try:
      kwargs = json.loads(call.arguments or "{}")
      original_args_str = json.dumps(kwargs)
      validation = validate_tool_arguments(registered.schema.parameters, kwargs)
      if validation.get("error"):
        yield ToolResultEvent(
          call_id=call.id,
          name=call.name,
          content=f"invalid arguments: {validation['error']}",
          is_error=True,
          is_fatal=False,
          error_kind="recoverable",
        )
        return
      if validation.get("repaired"):
        yield ToolArgumentRepairedEvent(
          call_id=call.id,
          name=call.name,
          original_arguments=original_args_str,
          repaired_arguments=json.dumps(kwargs),
        )
      # M3/G4: pass the run context (incl. ``cwd``, ``audit``) so cwd-aware / audit-aware tools
      # scope work to the worktree and route best-effort side-effects through the plane.
      output = await registered(_ctx=call_ctx, **kwargs)
      if isinstance(output, AsyncIterable):
        combined = ""
        iterator = output.__aiter__()
        resume_value = None
        while True:
          try:
            if resume_value is None:
              raw = await iterator.__anext__()
            else:
              raw = await iterator.asend(resume_value)
            resume_value = None
          except StopAsyncIteration:
            break
          chunk = normalize_tool_chunk(raw)
          if chunk.get("type") == "suspend":
            suspension_id = str(chunk.get("suspensionId", chunk.get("suspension_id", "")))
            event = ToolSuspendEvent(
              call_id=call.id, name=call.name, suspension_id=suspension_id,
              payload=chunk.get("payload"),
            )
            yield event
            if ctx.on_tool_suspend is None:
              yield ToolResultEvent(
                call_id=call.id,
                name=call.name,
                content=f"tool suspended without resume handler: {suspension_id}",
                is_error=True,
                is_fatal=False,
                error_kind="recoverable",
              )
              return
            resume_value = ctx.on_tool_suspend(event)
            if hasattr(resume_value, "__await__"):
              resume_value = await resume_value
            continue
          delta = tool_chunk_text(raw)
          combined += delta
          if delta:
            _maybe_warn_failure_shaped_chunk(call.name, delta)
          yield ToolDeltaEvent(
            call_id=call.id, name=call.name, delta=delta,
            chunk=None if isinstance(raw, str) else chunk,
          )
        for label, error in audit_failures:
          yield ToolAuditFailedEvent(call_id=call.id, name=call.name, label=label, error=error)
        yield ToolResultEvent(call_id=call.id, name=call.name, content=combined, is_error=False)
        return
      for label, error in audit_failures:
        yield ToolAuditFailedEvent(call_id=call.id, name=call.name, label=label, error=error)
      yield ToolResultEvent(call_id=call.id, name=call.name, content=str(output), is_error=False)
    except Exception as exc:
      is_fatal = getattr(exc, "is_fatal", False)
      error_kind = getattr(exc, "error_kind", None)
      if error_kind is None:
        error_kind = "fatal" if is_fatal else "recoverable"
      for label, error in audit_failures:
        yield ToolAuditFailedEvent(call_id=call.id, name=call.name, label=label, error=error)
      yield ToolResultEvent(
        call_id=call.id,
        name=call.name,
        content=format_tool_error(exc),
        is_error=True,
        is_fatal=is_fatal,
        error_kind=error_kind,
      )


def _parse_json(s: str) -> dict:
  try:
    return json.loads(s) if isinstance(s, str) else dict(s)
  except Exception:
    return {}


async def resolve_permission_request(request: PermissionRequestEvent, ctx: RunContext) -> PermissionResponse:
  return await _resolve_permission_request(request, ctx)


async def _resolve_permission_request(request: PermissionRequestEvent, ctx: RunContext) -> PermissionResponse:
  if ctx.on_permission_request is None:
    return PermissionResponse(
      approved=False,
      responder="policy_gate",
      reason="no permission handler configured",
    )

  try:
    value = ctx.on_permission_request(request)
    if hasattr(value, "__await__"):
      value = await value
    return _normalize_permission_response(value)
  except Exception as exc:
    return PermissionResponse(
      approved=False,
      responder="permission_handler",
      reason=f"permission handler failed: {exc}",
    )


def _normalize_permission_response(value: PermissionResponse | bool | dict[str, Any]) -> PermissionResponse:
  if isinstance(value, bool):
    return PermissionResponse(approved=value, responder="host")
  if isinstance(value, PermissionResponse):
    return PermissionResponse(
      approved=bool(value.approved),
      responder=value.responder or "host",
      reason=value.reason,
    )
  return PermissionResponse(
    approved=bool(value.get("approved")),
    responder=str(value.get("responder") or "host"),
    reason=str(value["reason"]) if value.get("reason") is not None else None,
  )
