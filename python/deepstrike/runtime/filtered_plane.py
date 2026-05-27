from __future__ import annotations

from collections.abc import AsyncIterator

from deepstrike._kernel import ToolCall
from deepstrike.providers.stream import StreamEvent, ToolDeniedEvent, ToolResultEvent
from deepstrike.runtime.execution_plane import ExecutionPlane, RunContext

_DEFAULT_META = frozenset({"skill", "memory", "knowledge", "update_plan"})


class FilteredExecutionPlane(ExecutionPlane):
  """Wraps an execution plane, allowing only manifest-permitted tool IDs (+ meta-tools)."""

  def __init__(
    self,
    inner: ExecutionPlane,
    permitted_ids: set[str],
    meta_tools: frozenset[str] = _DEFAULT_META,
  ) -> None:
    self._inner = inner
    self._permitted = permitted_ids
    self._meta = meta_tools

  def register(self, *tools) -> "FilteredExecutionPlane":
    self._inner.register(*tools)
    return self

  def unregister(self, name: str) -> "FilteredExecutionPlane":
    self._inner.unregister(name)
    return self

  def schemas(self) -> list[dict]:
    return [
      s for s in self._inner.schemas()
      if s.name in self._permitted or s.name in self._meta
    ]

  async def execute_all(self, calls: list[ToolCall], ctx: RunContext) -> AsyncIterator[StreamEvent]:
    permitted: list[ToolCall] = []
    for call in calls:
      if call.name in self._meta or call.name in self._permitted:
        permitted.append(call)
        continue
      reason = f"capability not permitted for sub-agent: {call.name}"
      yield ToolDeniedEvent(call_id=call.id, tool_name=call.name, reason=reason)
      yield ToolResultEvent(
        call_id=call.id,
        name=call.name,
        content=reason,
        is_error=True,
        error_kind="governance_denied",
      )
    if permitted:
      async for evt in self._inner.execute_all(permitted, ctx):
        yield evt
