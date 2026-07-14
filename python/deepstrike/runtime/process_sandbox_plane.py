from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path

from deepstrike._kernel import ToolSchema
from deepstrike.tools.errors import format_tool_error
from deepstrike.tools.registry import RegisteredTool
from deepstrike.runtime.execution_plane import LocalExecutionPlane
from deepstrike.runtime.reliability import OperationContext


class ProcessSandboxPlane(LocalExecutionPlane):
  """
  LocalExecutionPlane extended with two subprocess tools:
    - run_bash  — executes a bash command inside sandboxDir.
    - run_python — evaluates a Python script inside sandboxDir.

  Subprocesses run with sandboxDir as cwd and a stripped environment.
  This is execution hygiene, not an OS-enforced filesystem sandbox.
  JS-registered tools still run in-process (same as LocalExecutionPlane).
  """

  def __init__(
    self,
    *,
    sandbox_dir: str | Path,
    allowed_env_keys: list[str] | None = None,
    timeout_ms: int = 30_000,
    max_output_bytes: int = 1_048_576,
  ) -> None:
    super().__init__()
    self._sandbox_dir = Path(sandbox_dir)
    self._allowed_env_keys = allowed_env_keys or []
    self._timeout_s = timeout_ms / 1000
    self._max_output_bytes = max_output_bytes

    self.register(self._make_bash_tool(), self._make_python_tool())

  def _build_env(self) -> dict[str, str]:
    env: dict[str, str] = {
      "HOME": str(self._sandbox_dir),
      "TMPDIR": str(self._sandbox_dir),
      "PATH": "/usr/local/bin:/usr/bin:/bin",
    }
    for key in self._allowed_env_keys:
      if key in os.environ:
        env[key] = os.environ[key]
    return env

  async def _run_subprocess(
    self, cmd: str, args: list[str], cwd: str | None = None,
    operation: OperationContext | None = None,
  ) -> tuple[str, bool]:
    # M3/G4: run in the sub-agent's worktree when one was injected, else the sandbox dir.
    effective_cwd = cwd or str(self._sandbox_dir)
    if cwd is None:
      self._sandbox_dir.mkdir(parents=True, exist_ok=True)
    try:
      proc = await asyncio.create_subprocess_exec(
        cmd, *args,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=effective_cwd,
        env=self._build_env(),
      )
      communicate = asyncio.create_task(proc.communicate())
      cancel_waiter = (
        asyncio.create_task(operation.cancelled.wait())
        if operation is not None and operation.cancelled is not None else None
      )
      timeout_s = self._timeout_s
      if operation is not None and operation.deadline_ms is not None:
        import time
        timeout_s = min(timeout_s, (operation.deadline_ms - int(time.time() * 1000)) / 1000)
      done, _ = await asyncio.wait(
        {communicate, *([cancel_waiter] if cancel_waiter is not None else [])},
        timeout=max(0, timeout_s), return_when=asyncio.FIRST_COMPLETED,
      )
      if communicate not in done:
        proc.kill()
        await proc.wait()
        communicate.cancel()
        await asyncio.gather(communicate, return_exceptions=True)
        if cancel_waiter is not None:
          cancel_waiter.cancel()
          await asyncio.gather(cancel_waiter, return_exceptions=True)
        reason = "operation cancelled" if cancel_waiter is not None and cancel_waiter in done else "operation deadline exceeded"
        return reason, True
      stdout, stderr = await communicate
      if cancel_waiter is not None:
        cancel_waiter.cancel()
        await asyncio.gather(cancel_waiter, return_exceptions=True)

      combined = stdout + stderr
      if len(combined) > self._max_output_bytes:
        combined = combined[: self._max_output_bytes] + b"\n[output truncated]"
      return combined.decode("utf-8", errors="replace"), proc.returncode != 0
    except Exception as exc:
      return format_tool_error(exc), True

  def _make_bash_tool(self) -> RegisteredTool:
    sandbox = self

    async def run_bash(command: str, ctx=None) -> str:
      cwd = ctx.cwd if ctx is not None and ctx.cwd else None
      operation = ctx.operation if ctx is not None else None
      output, is_error = await sandbox._run_subprocess("bash", ["-c", command], cwd, operation)
      if is_error and not output.strip():
        return "Process exited with non-zero status and produced no output."
      return output or "(no output)"

    schema = ToolSchema(
      name="run_bash",
      description="Run a bash command with the sandbox directory as cwd and a stripped environment.",
      parameters=json.dumps({
        "type": "object",
        "properties": {"command": {"type": "string", "description": "The bash command to execute."}},
        "required": ["command"],
      }),
    )
    return RegisteredTool(run_bash, schema)

  def _make_python_tool(self) -> RegisteredTool:
    sandbox = self

    async def run_python(code: str, ctx=None) -> str:
      cwd = ctx.cwd if ctx is not None and ctx.cwd else None
      operation = ctx.operation if ctx is not None else None
      output, is_error = await sandbox._run_subprocess("python3", ["-c", code], cwd, operation)
      if is_error and not output.strip():
        return "Script exited with non-zero status and produced no output."
      return output or "(no output)"

    schema = ToolSchema(
      name="run_python",
      description="Evaluate a Python script with the sandbox directory as cwd and a stripped environment.",
      parameters=json.dumps({
        "type": "object",
        "properties": {"code": {"type": "string", "description": "The Python code to evaluate."}},
        "required": ["code"],
      }),
    )
    return RegisteredTool(run_python, schema)
