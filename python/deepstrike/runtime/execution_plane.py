from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterable, AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Awaitable, Callable

from deepstrike._kernel import ToolCall, ToolResult, ToolSchema
from deepstrike.providers.stream import (
  PermissionRequestEvent,
  StreamEvent,
  ToolDeltaEvent,
  ToolDeniedEvent,
  ToolResultEvent,
  ToolSuspendEvent,
  ToolArgumentRepairedEvent,
)
from deepstrike.tools.registry import RegisteredTool, normalize_tool_chunk, tool_chunk_text, validate_tool_arguments

if TYPE_CHECKING:
  from deepstrike.governance import Governance
  from deepstrike.knowledge.source import KnowledgeSource
  from deepstrike.memory.protocols import DreamStore


def _strip_frontmatter(content: str) -> str:
  import re
  return re.sub(r"^---\n.*?\n---\n?", "", content, count=1, flags=re.DOTALL)


@dataclass
class RunContext:
  agent_id: str | None = None
  skill_dir: Path | None = None
  dream_store: "DreamStore | None" = None
  knowledge_source: "KnowledgeSource | None" = None
  governance: "Governance | None" = None
  on_tool_suspend: Callable[[ToolSuspendEvent], Awaitable[Any] | Any] | None = None


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
    permitted: list[ToolCall] = []
    for c in calls:
      if ctx.governance:
        import time
        ctx.governance.set_time(int(time.time() * 1000))
        verdict = ctx.governance.evaluate(c.name, c.arguments)
        if verdict.kind == "deny":
          yield ToolDeniedEvent(call_id=c.id, tool_name=c.name, reason=verdict.reason or "")
          yield ToolResultEvent(
            call_id=c.id,
            name=c.name,
            content=f"permission denied: {verdict.reason or ''}",
            is_error=True,
            is_fatal=False,
            error_kind="governance_denied",
          )
          continue
        if verdict.kind == "rate_limited":
          msg = f"rate limited: {c.name}"
          yield ToolResultEvent(
            call_id=c.id,
            name=c.name,
            content=msg,
            is_error=True,
            is_fatal=False,
            error_kind="recoverable",
          )
          continue
        if verdict.kind == "ask_user":
          yield PermissionRequestEvent(
            call_id=c.id, tool_name=c.name, arguments=c.arguments, reason=verdict.reason or "",
          )
          yield ToolResultEvent(
            call_id=c.id,
            name=c.name,
            content="awaiting user approval",
            is_error=True,
            is_fatal=False,
            error_kind="recoverable",
          )
          continue
      permitted.append(c)

    skill_calls = [c for c in permitted if c.name == "skill"]
    memory_calls = [c for c in permitted if c.name == "memory"]
    knowledge_calls = [c for c in permitted if c.name == "knowledge"]
    regular_calls = [c for c in permitted if c.name not in ("skill", "memory", "knowledge")]

    for c in skill_calls:
      args = _parse_json(c.arguments)
      name = str(args.get("name", ""))
      content = None
      if ctx.skill_dir and name:
        path = ctx.skill_dir / f"{name}.md"
        if path.exists():
          content = _strip_frontmatter(path.read_text(encoding="utf-8"))
      output = content if content is not None else f'Skill "{name}" not found.'
      yield ToolResultEvent(call_id=c.id, name=c.name, content=output, is_error=content is None)

    for c in memory_calls:
      if ctx.dream_store and ctx.agent_id:
        args = _parse_json(c.arguments)
        query = str(args.get("query", ""))
        top_k = int(args.get("top_k", 5))
        entries = await ctx.dream_store.search(ctx.agent_id, query, top_k)
        output = (
          "\n---\n".join(f"[score={e.score:.3f}] {e.text}" for e in entries)
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

  async def _execute_single(self, call: ToolCall, ctx: RunContext) -> AsyncIterator[StreamEvent]:
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
      output = await registered(**kwargs)
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
          yield ToolDeltaEvent(
            call_id=call.id, name=call.name, delta=delta,
            chunk=None if isinstance(raw, str) else chunk,
          )
        yield ToolResultEvent(call_id=call.id, name=call.name, content=combined, is_error=False)
        return
      yield ToolResultEvent(call_id=call.id, name=call.name, content=str(output), is_error=False)
    except Exception as exc:
      is_fatal = getattr(exc, "is_fatal", False)
      error_kind = getattr(exc, "error_kind", None)
      if error_kind is None:
        error_kind = "fatal" if is_fatal else "recoverable"
      yield ToolResultEvent(
        call_id=call.id,
        name=call.name,
        content=str(exc),
        is_error=True,
        is_fatal=is_fatal,
        error_kind=error_kind,
      )


def _parse_json(s: str) -> dict:
  try:
    return json.loads(s) if isinstance(s, str) else dict(s)
  except Exception:
    return {}
