"""M3/G4: per-sub-agent git-worktree isolation as an execution-plane decorator (Python mirror of the
Node ``worktree-plane.ts``).

An ``isolation: "worktree"`` workflow node runs its tools in its own working tree so parallel
write-capable nodes don't clobber each other. This owns the worktree *lifecycle*: create one git
worktree per sub-agent, inject its path as ``RunContext.cwd`` so a cwd-aware tool scopes its work
there, and remove it when the sub-agent finishes. The git operations are behind an injectable
``WorktreeManager`` so the plane is testable without mutating a real repository.
"""

from __future__ import annotations

import asyncio
import re
import tempfile
from dataclasses import replace
from pathlib import Path
from typing import Protocol

from deepstrike.runtime.execution_plane import RunContext


class WorktreeManager(Protocol):
  """Creates and removes the worktree directory for one sub-agent (injectable for testing)."""

  async def create(self, agent_id: str) -> str: ...
  async def remove(self, path: str) -> None: ...


class GitWorktreeManager:
  """Default git-backed manager: ``git worktree add --detach <root>/<id> <ref>`` then
  ``git worktree remove --force`` (falling back to a recursive delete). Cleanup never raises."""

  def __init__(self, repo_root: str | None = None, ref: str = "HEAD", root_dir: str | None = None) -> None:
    self._repo_root = repo_root
    self._ref = ref
    self._root_dir = root_dir

  async def create(self, agent_id: str) -> str:
    root = self._root_dir or tempfile.mkdtemp(prefix="deepstrike-wt-")
    # ``agent_id`` is a kernel-generated ``wf-node{N}`` id, but sanitize defensively.
    safe = re.sub(r"[^A-Za-z0-9_-]", "_", agent_id)
    path = str(Path(root) / safe)
    await self._git("worktree", "add", "--detach", path, self._ref)
    return path

  async def remove(self, path: str) -> None:
    try:
      await self._git("worktree", "remove", "--force", path)
    except Exception:  # noqa: BLE001 — best-effort cleanup; fall back to a plain delete.
      import shutil
      shutil.rmtree(path, ignore_errors=True)

  async def _git(self, *args: str) -> None:
    proc = await asyncio.create_subprocess_exec(
      "git", *args,
      stdout=asyncio.subprocess.PIPE,
      stderr=asyncio.subprocess.PIPE,
      cwd=self._repo_root,
    )
    _out, err = await proc.communicate()
    if proc.returncode != 0:
      raise RuntimeError(f"git {' '.join(args)} failed: {err.decode('utf-8', 'replace')}")


class WorktreeExecutionPlane:
  """Decorator plane: lazily creates a worktree on first execution, injects it as ``RunContext.cwd``
  for every delegated call, and removes it on ``cleanup``. Registration/schemas pass through. The
  worktree only *isolates* to the extent the inner plane's tools honor ``ctx.cwd``."""

  def __init__(self, inner, manager: WorktreeManager, agent_id: str) -> None:
    self._inner = inner
    self._manager = manager
    self._agent_id = agent_id
    self._path: str | None = None

  def register(self, *tools):
    self._inner.register(*tools)
    return self

  def unregister(self, name: str):
    self._inner.unregister(name)
    return self

  def schemas(self):
    return self._inner.schemas()

  def worktree_path(self) -> str | None:
    return self._path

  async def execute_all(self, calls, ctx: RunContext):
    if self._path is None:
      self._path = await self._manager.create(self._agent_id)
    async for evt in self._inner.execute_all(calls, replace(ctx, cwd=self._path)):
      yield evt

  async def cleanup(self) -> None:
    if self._path is None:
      return
    path, self._path = self._path, None
    await self._manager.remove(path)
