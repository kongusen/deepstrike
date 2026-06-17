from __future__ import annotations

import asyncio
import json
import os
from asyncio.subprocess import PIPE
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from deepstrike._kernel import ToolCall, ToolSchema
from deepstrike.providers.stream import StreamEvent, ToolResultEvent
from deepstrike.tools.errors import format_tool_error
from deepstrike.tools.registry import RegisteredTool
from deepstrike.runtime.execution_plane import ExecutionPlane, LocalExecutionPlane, RunContext
from deepstrike.runtime.credential_vault import CredentialVault

if TYPE_CHECKING:
  pass


@dataclass
class McpServerConfig:
  command: str
  args: list[str] = field(default_factory=list)
  credential_keys: list[str] = field(default_factory=list)
  env: dict[str, str] = field(default_factory=dict)


# ── Internal MCP client ───────────────────────────────────────────────────────

class _McpConnection:
  def __init__(self, server_name: str, config: McpServerConfig, vault: CredentialVault) -> None:
    self._name = server_name
    self._config = config
    self._vault = vault
    self._proc: asyncio.subprocess.Process | None = None
    self._pending: dict[int, asyncio.Future[Any]] = {}
    self._next_id = 1
    self._schemas: list[ToolSchema] = []
    self._schema_names: set[str] = set()
    self._reader_task: asyncio.Task | None = None

  async def start(self) -> None:
    env = {**os.environ, **self._config.env}
    for key in self._config.credential_keys:
      val = await self._vault.get(key)
      if val is not None:
        env[key] = val

    self._proc = await asyncio.create_subprocess_exec(
      self._config.command, *self._config.args,
      stdin=PIPE, stdout=PIPE, stderr=None,
      env=env,
    )
    self._reader_task = asyncio.create_task(self._read_loop())

    # MCP handshake
    await self._request("initialize", {
      "protocolVersion": "2024-11-05",
      "capabilities": {"tools": {}},
      "clientInfo": {"name": "deepstrike", "version": "0.1.0"},
    })
    self._notify("notifications/initialized")

    result: dict = await self._request("tools/list")  # type: ignore[assignment]
    for t in result.get("tools", []):
      schema = ToolSchema(
        name=t["name"],
        description=t.get("description", t["name"]),
        parameters=json.dumps(t.get("inputSchema", {"type": "object", "properties": {}})),
      )
      self._schemas.append(schema)
      self._schema_names.add(t["name"])

  async def _read_loop(self) -> None:
    assert self._proc and self._proc.stdout
    while True:
      line = await self._proc.stdout.readline()
      if not line:
        break
      try:
        msg = json.loads(line.decode())
        msg_id = msg.get("id")
        fut = self._pending.pop(msg_id, None) if msg_id is not None else None
        if fut and not fut.done():
          if "error" in msg:
            e = msg["error"]
            fut.set_exception(RuntimeError(f"MCP({self._name}) {e.get('code')}: {e.get('message')}"))
          else:
            fut.set_result(msg.get("result"))
      except Exception:
        pass

  def _request(self, method: str, params: Any = None) -> asyncio.Future:
    assert self._proc and self._proc.stdin
    rpc_id = self._next_id
    self._next_id += 1
    fut: asyncio.Future = asyncio.get_event_loop().create_future()
    self._pending[rpc_id] = fut
    msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method, "id": rpc_id}
    if params is not None:
      msg["params"] = params
    self._proc.stdin.write((json.dumps(msg) + "\n").encode())
    return fut

  def _notify(self, method: str, params: Any = None) -> None:
    if not (self._proc and self._proc.stdin):
      return
    msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
    if params is not None:
      msg["params"] = params
    self._proc.stdin.write((json.dumps(msg) + "\n").encode())

  def schemas(self) -> list[ToolSchema]:
    return self._schemas

  def has_schema(self, name: str) -> bool:
    return name in self._schema_names

  async def execute(self, call: ToolCall) -> tuple[str, bool]:
    try:
      args = json.loads(call.arguments or "{}")
      result: dict = await self._request("tools/call", {"name": call.name, "arguments": args})
      text = "\n".join(
        c.get("text", "") for c in result.get("content", []) if c.get("type") == "text"
      )
      return (text or json.dumps(result)), result.get("isError", False)
    except Exception as exc:
      return format_tool_error(exc), True

  async def stop(self) -> None:
    if self._reader_task:
      self._reader_task.cancel()
    if self._proc:
      self._proc.kill()
      self._proc = None
    for fut in self._pending.values():
      if not fut.done():
        fut.set_exception(RuntimeError(f"MCP server '{self._name}' stopped"))
    self._pending.clear()


# ── Public plane ──────────────────────────────────────────────────────────────

class McpProxyPlane:
  """
  ExecutionPlane that proxies tool calls to MCP servers.

  Credentials live in a CredentialVault and are injected into each server's
  subprocess env — the model never sees the credential values.

  Usage:
    plane = McpProxyPlane(
      servers={"brave": McpServerConfig(command="npx", args=["-y", "@mcp/server-brave-search"], credential_keys=["BRAVE_API_KEY"])},
      vault=EnvCredentialVault(),
    )
    await plane.connect()
    # ... use with RuntimeRunner ...
    await plane.disconnect()
  """

  def __init__(self, *, servers: dict[str, McpServerConfig], vault: CredentialVault) -> None:
    self._server_configs = servers
    self._vault = vault
    self._connections: dict[str, _McpConnection] = {}
    self._tool_to_conn: dict[str, _McpConnection] = {}
    self._local = LocalExecutionPlane()
    self._local_names: set[str] = set()

  async def connect(self) -> None:
    for name, config in self._server_configs.items():
      conn = _McpConnection(name, config, self._vault)
      await conn.start()
      self._connections[name] = conn
      for schema in conn.schemas():
        self._tool_to_conn[schema.name] = conn

  async def disconnect(self) -> None:
    for conn in self._connections.values():
      await conn.stop()
    self._connections.clear()
    self._tool_to_conn.clear()

  def register(self, *tools: RegisteredTool) -> "McpProxyPlane":
    self._local.register(*tools)
    for t in tools:
      self._local_names.add(t.schema.name)
    return self

  def unregister(self, name: str) -> "McpProxyPlane":
    self._local.unregister(name)
    self._local_names.discard(name)
    return self

  def schemas(self) -> list[ToolSchema]:
    mcp: list[ToolSchema] = []
    for conn in self._connections.values():
      mcp.extend(conn.schemas())
    return [*self._local.schemas(), *mcp]

  async def execute_all(self, calls: list[ToolCall], ctx: RunContext) -> AsyncIterator[StreamEvent]:
    local_calls = [c for c in calls if c.name in self._local_names]
    mcp_calls   = [c for c in calls if c.name not in self._local_names]

    if local_calls:
      async for evt in self._local.execute_all(local_calls, ctx):
        yield evt

    # Group by connection, run groups concurrently
    groups: dict[_McpConnection, list[ToolCall]] = {}
    unknown: list[ToolCall] = []
    for call in mcp_calls:
      conn = self._tool_to_conn.get(call.name)
      if conn is None:
        unknown.append(call)
      else:
        groups.setdefault(conn, []).append(call)

    for call in unknown:
      yield ToolResultEvent(call_id=call.id, name=call.name, content=f"unknown MCP tool: {call.name}", is_error=True)

    async def run_group(conn: _McpConnection, group: list[ToolCall]) -> list[tuple[ToolCall, str, bool]]:
      results = []
      for call in group:
        output, is_error = await conn.execute(call)
        results.append((call, output, is_error))
      return results

    tasks = [asyncio.create_task(run_group(conn, group)) for conn, group in groups.items()]
    for task in tasks:
      for call, output, is_error in await task:
        yield ToolResultEvent(call_id=call.id, name=call.name, content=output, is_error=is_error)
