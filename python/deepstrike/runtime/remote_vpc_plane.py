from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from typing import Any

import aiohttp

from deepstrike._kernel import ToolCall, ToolSchema
from deepstrike.providers.stream import StreamEvent, ToolResultEvent
from deepstrike.tools.errors import format_tool_error
from deepstrike.tools.registry import RegisteredTool
from deepstrike.runtime.execution_plane import LocalExecutionPlane, RunContext
from deepstrike.runtime.credential_vault import CredentialVault


class RemoteVpcPlane:
  """
  ExecutionPlane that forwards tool calls over HTTP to a worker inside a customer VPC.

  Credentials are fetched from the vault at call time and injected into HTTP headers —
  they are never forwarded to the model or stored in the session log.

  The remote worker must expose:
    POST {base_url}/execute   body: { name, arguments }   response: { output, isError }

  Local tools registered via register() run in-process and take priority over any
  remote schema with the same name.
  """

  def __init__(
    self,
    *,
    base_url: str,
    vault: CredentialVault,
    schemas: list[ToolSchema],
    auth_credential_key: str | None = None,
    timeout_ms: int = 30_000,
  ) -> None:
    self._base_url = base_url.rstrip("/")
    self._vault = vault
    self._remote_schemas = schemas
    self._remote_names = {s.name for s in schemas}
    self._auth_key = auth_credential_key
    self._timeout_s = timeout_ms / 1000
    self._local = LocalExecutionPlane()

  def register(self, *tools: RegisteredTool) -> "RemoteVpcPlane":
    self._local.register(*tools)
    return self

  def unregister(self, name: str) -> "RemoteVpcPlane":
    self._local.unregister(name)
    return self

  def schemas(self) -> list[ToolSchema]:
    local_names = {s.name for s in self._local.schemas()}
    remote_visible = [s for s in self._remote_schemas if s.name not in local_names]
    return [*self._local.schemas(), *remote_visible]

  async def execute_all(self, calls: list[ToolCall], ctx: RunContext) -> AsyncIterator[StreamEvent]:
    local_names = {s.name for s in self._local.schemas()}
    local_calls  = [c for c in calls if c.name in local_names]
    remote_calls = [c for c in calls if c.name not in local_names]

    if local_calls:
      async for evt in self._local.execute_all(local_calls, ctx):
        yield evt

    if not remote_calls:
      return

    auth = await self._vault.get(self._auth_key) if self._auth_key else None
    headers: dict[str, str] = {"Content-Type": "application/json"}
    if auth:
      headers["Authorization"] = auth

    # Fire all remote calls concurrently; yield in dispatch order
    tasks = [asyncio.create_task(self._call_remote(call, headers)) for call in remote_calls]
    for call, task in zip(remote_calls, tasks):
      output, is_error = await task
      yield ToolResultEvent(call_id=call.id, name=call.name, content=output, is_error=is_error)

  async def _call_remote(self, call: ToolCall, headers: dict[str, str]) -> tuple[str, bool]:
    try:
      args: Any = json.loads(call.arguments or "{}")
      timeout = aiohttp.ClientTimeout(total=self._timeout_s)
      async with aiohttp.ClientSession(timeout=timeout) as session:
        async with session.post(
          f"{self._base_url}/execute",
          json={"name": call.name, "arguments": args},
          headers=headers,
        ) as resp:
          if not resp.ok:
            body = await resp.text()
            return f"HTTP {resp.status}{': ' + body if body else ''}", True
          result: dict = await resp.json()
          return result.get("output", ""), bool(result.get("isError", False))
    except Exception as exc:
      return format_tool_error(exc), True
