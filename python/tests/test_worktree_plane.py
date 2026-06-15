import json

import pytest

from deepstrike.runtime.execution_plane import LocalExecutionPlane, RunContext
from deepstrike.runtime.worktree_plane import WorktreeExecutionPlane
from deepstrike.tools.registry import tool
from deepstrike._kernel import ToolCall


class _FakeManager:
    """Records create/remove; hands back a deterministic fake path (no real git)."""

    def __init__(self) -> None:
        self.created: list[str] = []
        self.removed: list[str] = []

    async def create(self, agent_id: str) -> str:
        self.created.append(agent_id)
        return f"/tmp/wt/{agent_id}"

    async def remove(self, path: str) -> None:
        self.removed.append(path)


class _RecordingPlane:
    """Inner plane that records the cwd each execute_all was given."""

    def __init__(self) -> None:
        self.cwds: list[str | None] = []

    def register(self, *tools):
        return self

    def unregister(self, name):
        return self

    def schemas(self):
        return []

    async def execute_all(self, calls, ctx):
        self.cwds.append(ctx.cwd)
        if False:  # make this an async generator that yields nothing
            yield None


@pytest.mark.asyncio
async def test_worktree_plane_lifecycle():
    mgr = _FakeManager()
    inner = _RecordingPlane()
    wt = WorktreeExecutionPlane(inner, mgr, "wf-node3")

    assert wt.worktree_path() is None
    async for _ in wt.execute_all([], RunContext()):
        pass
    async for _ in wt.execute_all([], RunContext()):
        pass

    assert mgr.created == ["wf-node3"]  # created exactly once
    assert wt.worktree_path() == "/tmp/wt/wf-node3"
    assert inner.cwds == ["/tmp/wt/wf-node3", "/tmp/wt/wf-node3"]  # cwd injected each call

    await wt.cleanup()
    assert mgr.removed == ["/tmp/wt/wf-node3"]
    assert wt.worktree_path() is None
    await wt.cleanup()  # idempotent
    assert mgr.removed == ["/tmp/wt/wf-node3"]


@pytest.mark.asyncio
async def test_injected_cwd_reaches_a_tool_via_ctx():
    # Full thread: WorktreeExecutionPlane injects ctx.cwd → LocalExecutionPlane passes ctx →
    # the tool's `ctx` param receives it. This is what makes worktree isolation real.
    seen: dict[str, str | None] = {}

    async def probe(ctx=None) -> str:
        seen["cwd"] = ctx.cwd if ctx is not None else None
        return "ok"

    rt = tool(probe)
    # `ctx` must never appear as a tool argument in the schema.
    assert "ctx" not in json.loads(rt.schema.parameters)["properties"]

    inner = LocalExecutionPlane().register(rt)
    wt = WorktreeExecutionPlane(inner, _FakeManager(), "wf-node7")
    async for _ in wt.execute_all([ToolCall(id="c1", name="probe", arguments="{}")], RunContext()):
        pass

    assert seen["cwd"] == "/tmp/wt/wf-node7"


@pytest.mark.asyncio
async def test_tool_without_ctx_param_is_unaffected():
    async def plain(command: str) -> str:
        return command

    rt = tool(plain)
    # A tool that doesn't declare `ctx` still runs normally even when a context is passed.
    assert await rt(_ctx=RunContext(cwd="/y"), command="hi") == "hi"
