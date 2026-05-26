from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path

from deepstrike._kernel import ToolSchema
from deepstrike.tools.registry import RegisteredTool
from deepstrike.runtime.execution_plane import LocalExecutionPlane


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

  async def _run_subprocess(self, cmd: str, args: list[str]) -> tuple[str, bool]:
    self._sandbox_dir.mkdir(parents=True, exist_ok=True)
    try:
      proc = await asyncio.create_subprocess_exec(
        cmd, *args,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=str(self._sandbox_dir),
        env=self._build_env(),
      )
      try:
        stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=self._timeout_s)
      except asyncio.TimeoutError:
        proc.kill()
        return f"timed out after {int(self._timeout_s * 1000)}ms", True

      combined = stdout + stderr
      if len(combined) > self._max_output_bytes:
        combined = combined[: self._max_output_bytes] + b"\n[output truncated]"
      return combined.decode("utf-8", errors="replace"), proc.returncode != 0
    except Exception as exc:
      return str(exc), True

  def _make_bash_tool(self) -> RegisteredTool:
    sandbox = self

    async def run_bash(command: str) -> str:
      output, is_error = await sandbox._run_subprocess("bash", ["-c", command])
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

    async def run_python(code: str) -> str:
      output, is_error = await sandbox._run_subprocess("python3", ["-c", code])
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
