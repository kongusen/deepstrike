# Execution Plane & Tools

ExecutionPlane is DeepStrike's tool execution layer. The kernel adjudicates tool syscalls, records observations, and maintains context; actual function calls, subprocesses, remote HTTP calls, and worktree cwd injection happen in the SDK ExecutionPlane.

**Code entry points**:

- `python/deepstrike/runtime/execution_plane.py`
- `python/deepstrike/tools/registry.py`
- `python/deepstrike/runtime/worktree_plane.py`
- `python/deepstrike/runtime/process_sandbox_plane.py`
- `python/deepstrike/runtime/remote_vpc_plane.py`
- `python/deepstrike/runtime/large_result_spool.py`

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the kernel | Receives approved tool calls and writes results back as observations |
| To the host | Binds Python functions, subprocesses, worktrees, remote VPCs, or customer environments |
| To governance | Honors schema filtering, permission, quota, and sandbox decisions |
| To the Context VM | Projects large results through spool / handles instead of flooding context |

The ExecutionPlane is the OS device-driver layer: the kernel does not directly read or write the outside world; it delegates approved actions to the host through this plane.

![Execution Plane Mechanisms](/execution_plane_mechanisms.svg)

## When to Customize ExecutionPlane

| Need | Use |
|------|-----|
| Register normal Python tools | `LocalExecutionPlane().register(tool(...))` |
| Stream tool output | `streaming_tool` / async iterable chunks |
| Wait for external resume | yield `{"type": "suspend", ...}` and configure `on_tool_suspend` |
| Write files | make tools honor `ctx.cwd`, then use worktree / sandbox |
| Handle huge outputs | configure `LargeResultSpool` |
| Execute inside customer VPC | `RemoteVpcPlane` |
| Expose only a tool subset | `FilteredExecutionPlane`, Skill gating, Governance |

## Level 1: Register Local Tools

```python
from deepstrike import LocalExecutionPlane, RuntimeOptions, RuntimeRunner, tool

@tool("read_ticket", "Read a ticket by id")
async def read_ticket(id: str) -> str:
    return f"ticket {id}: ..."

plane = LocalExecutionPlane().register(read_ticket)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    execution_plane=plane,
))
```

`LocalExecutionPlane.schemas()` gives tool schemas to the kernel. The kernel exposes only schemas that pass governance and capability gating.

## Level 2: Argument Validation and Repair

Tool arguments are validated against JSON Schema:

- invalid schema / missing required field → `ToolResultEvent(is_error=True, error_kind="recoverable")`
- repairable arguments → emit `ToolArgumentRepairedEvent`
- tool exception → runtime catches and formats the tool error

```python
from deepstrike import tool

@tool(
    "resize",
    "Resize an image",
    parameters={
        "type": "object",
        "properties": {"size": {"type": "integer"}},
        "required": ["size"],
    },
)
async def resize(size: int) -> str:
    return f"resized to {size}"
```

## Level 3: Streaming Tools and Suspend

Streaming tools can return async iterable chunks. Supported chunks include text, progress, artifact, json_patch, and suspend.

```python
from deepstrike import streaming_tool

@streaming_tool("long_job", "Run a long job")
async def long_job():
    yield {"type": "progress", "progress": 0.3, "message": "started"}
    yield "partial output\n"
    yield {"type": "suspend", "suspensionId": "approve-1", "payload": {"reason": "need approval"}}
    yield "resumed\n"
```

Configure a resume callback:

```python
async def on_tool_suspend(event):
    return {"approved": True}

RuntimeOptions(..., on_tool_suspend=on_tool_suspend)
```

Without `on_tool_suspend`, the runtime returns a recoverable error instead of hanging.

## Level 4: Audit Side Effects

Tools can use `ctx.audit(label, fn)` for non-essential side effects such as audit logs or metrics. Audit failure emits `ToolAuditFailedEvent` and does not turn the main tool result into an error.

```python
@tool("write_record", "Write a record")
async def write_record(value: str, ctx=None) -> str:
    await save_record(value)

    if ctx and ctx.audit:
        await ctx.audit("metrics", lambda: emit_metric("record_written"))

    return "ok"
```

This avoids retries that duplicate an already-committed write because a metrics store failed.

## Level 5: Worktree Isolation

`isolation="worktree"` sub-agents need the host to create working directories. Python SDK provides `WorktreeExecutionPlane` and `GitWorktreeManager`:

```python
from deepstrike import GitWorktreeManager, RuntimeOptions

runner = RuntimeRunner(RuntimeOptions(
    ...,
    worktree_manager=GitWorktreeManager(repo_root="/repo", root_dir="/tmp/deepstrike-wt"),
))
```

Boundary:

- kernel declares `AgentIsolation::Worktree`
- SDK creates / removes the git worktree
- `WorktreeExecutionPlane` injects worktree path as `RunContext.cwd`
- tools must honor `ctx.cwd`; file access is not automatically isolated otherwise

## Level 6: Process Sandbox

`ProcessSandboxPlane` provides `run_bash` and `run_python` tools:

```python
from deepstrike.runtime.process_sandbox_plane import ProcessSandboxPlane

plane = ProcessSandboxPlane(
    sandbox_dir="./sandbox",
    allowed_env_keys=["PATH"],
    timeout_ms=30_000,
    max_output_bytes=1_048_576,
)
```

It uses the sandbox dir as cwd and strips environment variables. This is execution hygiene, not strong OS isolation; high-risk workloads should use containers, VMs, or remote sandboxes.

## Level 7: Remote VPC Tools

`RemoteVpcPlane` forwards tool calls to a remote worker:

```python
from deepstrike.runtime.remote_vpc_plane import RemoteVpcPlane

plane = RemoteVpcPlane(
    base_url="https://worker.internal",
    vault=vault,
    schemas=[remote_schema],
    auth_credential_key="worker-token",
)
```

The remote worker implements:

```text
POST /execute
body: { "name": "...", "arguments": { ... } }
response: { "output": "...", "isError": false }
```

Credentials are fetched from `CredentialVault` at call time and injected into HTTP headers. They do not enter model context or session log.

## Large Result Spool

When the kernel emits `large_result_spooled`, the SDK uses `LargeResultSpool` to persist the full output and keep a preview / ref in context:

```python
from deepstrike.runtime.large_result_spool import LargeResultSpool

RuntimeOptions(
    ...,
    result_spool=LargeResultSpool(".spool", max_age_seconds=7 * 24 * 3600),
)
```

If a read tool receives a `.spool/...` path argument, `LocalExecutionPlane` attempts to read the spooled result automatically.

## Kernel / Host Boundary

| Behavior | Owner |
|----------|-------|
| whether a tool schema is exposed | kernel + SDK capability gating |
| whether a tool call is allowed | kernel syscall / governance |
| Python function invocation | SDK ExecutionPlane |
| subprocess / HTTP / file writes | SDK / tool |
| whether a large result should spool | kernel decides, SDK writes |
| worktree lifecycle | SDK |

## Verification Entry Points

- `python/tests/test_streaming_tools.py`
- `python/tests/test_tool_argument_repair.py`
- `python/tests/test_large_result_spool.py`
- `python/tests/test_worktree_isolation.py`
- `node/tests/remote-vpc-plane.test.ts`
