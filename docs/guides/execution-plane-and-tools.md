# 执行平面与工具

ExecutionPlane 是 DeepStrike 的工具执行层。kernel 只裁决 tool syscall、记录 observation、维护上下文；真正的函数调用、进程启动、远程 HTTP、worktree cwd 注入都在 SDK 的 ExecutionPlane 中完成。

**代码入口**：

- `python/deepstrike/runtime/execution_plane.py`
- `python/deepstrike/tools/registry.py`
- `python/deepstrike/runtime/worktree_plane.py`
- `python/deepstrike/runtime/process_sandbox_plane.py`
- `python/deepstrike/runtime/remote_vpc_plane.py`
- `python/deepstrike/runtime/large_result_spool.py`

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 kernel | 接收已批准的 tool call，并把结果作为 observation 回写 |
| 对 host | 绑定 Python 函数、进程、worktree、远程 VPC 或客户环境 |
| 对治理面 | 尊重 schema filtering、permission、quota、sandbox 决策 |
| 对 Context VM | 大结果通过 spool / handle 投影，避免直接污染上下文 |

ExecutionPlane 是 OS 的“设备驱动层”：kernel 不直接读写外部世界，而是通过这个平面把批准后的动作交给宿主执行。

![Execution Plane Mechanisms](/execution_plane_mechanisms.svg)

## 什么时候需要自定义 ExecutionPlane

| 需求 | 推荐做法 |
|------|----------|
| 注册普通 Python 工具 | `LocalExecutionPlane().register(tool(...))` |
| 工具需要流式输出 | `streaming_tool` / async iterable chunk |
| 工具需要等待外部恢复 | yield `{"type": "suspend", ...}` 并配置 `on_tool_suspend` |
| 工具会写文件 | 让工具读取 `ctx.cwd`，配合 worktree / sandbox |
| 工具输出很大 | 配置 `LargeResultSpool` |
| 工具在客户 VPC 执行 | `RemoteVpcPlane` |
| 工具只应暴露一部分 | `FilteredExecutionPlane`、Skill gating、Governance |

## Level 1：注册本地工具

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

`LocalExecutionPlane.schemas()` 会把工具 schema 交给 kernel；kernel 在 `CallLLM` 时只暴露通过治理和能力门控的 schema。

## Level 2：工具参数校验与修复

工具参数先按 JSON Schema 校验：

- schema 不合法或必填缺失 → `ToolResultEvent(is_error=True, error_kind="recoverable")`
- 可修复参数 → emit `ToolArgumentRepairedEvent`
- 工具抛异常 → runtime 捕获并格式化为 tool error

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

## Level 3：流式工具与 suspend

流式工具可以返回 async iterable chunk。支持的 chunk 包括 text、progress、artifact、json_patch、suspend。

```python
from deepstrike import streaming_tool

@streaming_tool("long_job", "Run a long job")
async def long_job():
    yield {"type": "progress", "progress": 0.3, "message": "started"}
    yield "partial output\n"
    yield {"type": "suspend", "suspensionId": "approve-1", "payload": {"reason": "need approval"}}
    yield "resumed\n"
```

配置恢复回调：

```python
async def on_tool_suspend(event):
    # return value is sent back into the async generator
    return {"approved": True}

RuntimeOptions(..., on_tool_suspend=on_tool_suspend)
```

没有 `on_tool_suspend` 时，runtime 会返回 recoverable error，而不是永久卡住。

## Level 4：audit 副作用不能污染工具结果

工具可以使用 `ctx.audit(label, fn)` 包住非关键副作用，比如写审计日志、发 metrics。audit 失败会 emit `ToolAuditFailedEvent`，但不会把主工具结果改成 error。

```python
@tool("write_record", "Write a record")
async def write_record(value: str, ctx=None) -> str:
    # 关键写入
    await save_record(value)

    if ctx and ctx.audit:
        await ctx.audit("metrics", lambda: emit_metric("record_written"))

    return "ok"
```

这能避免“主操作已经成功，但审计系统失败导致 agent 重试并重复写入”的问题。

## Level 5：worktree 隔离

`isolation="worktree"` 的 sub-agent 需要宿主创建工作目录。Python SDK 提供 `WorktreeExecutionPlane` 和 `GitWorktreeManager`：

```python
from deepstrike import GitWorktreeManager, RuntimeOptions

runner = RuntimeRunner(RuntimeOptions(
    ...,
    worktree_manager=GitWorktreeManager(repo_root="/repo", root_dir="/tmp/deepstrike-wt"),
))
```

关键边界：

- kernel 只声明 `AgentIsolation::Worktree`
- SDK 创建 / 清理 git worktree
- `WorktreeExecutionPlane` 把 worktree path 注入 `RunContext.cwd`
- 工具必须主动使用 `ctx.cwd`，否则不会自动隔离文件访问

## Level 6：进程沙箱

`ProcessSandboxPlane` 提供 `run_bash` 和 `run_python` 两个工具：

```python
from deepstrike.runtime.process_sandbox_plane import ProcessSandboxPlane

plane = ProcessSandboxPlane(
    sandbox_dir="./sandbox",
    allowed_env_keys=["PATH"],
    timeout_ms=30_000,
    max_output_bytes=1_048_576,
)
```

它会使用 sandbox dir 作为 cwd，并裁剪环境变量。这是执行卫生，不是强 OS 级隔离；高风险场景仍应使用容器、VM 或远程沙箱。

ProcessSandbox、RemoteVpc 与 MCP adapter 都消费 `RunContext.operation`。operation 被取消或
deadline 早于 adapter 自身 timeout 时，SDK 会采用更早的边界：终止子进程、abort HTTP
请求或取消待处理 RPC，并清理对应 pending task。

## Level 7：远程 VPC 工具

`RemoteVpcPlane` 把 tool call 转发到远程 worker：

```python
from deepstrike.runtime.remote_vpc_plane import RemoteVpcPlane

plane = RemoteVpcPlane(
    base_url="https://worker.internal",
    vault=vault,
    schemas=[remote_schema],
    auth_credential_key="worker-token",
)
```

远程 worker 需要实现：

```text
POST /execute
body: { "name": "...", "arguments": { ... } }
response: { "output": "...", "isError": false }
```

凭据由 `CredentialVault` 在调用时注入 HTTP headers，不会进入模型上下文或 session log。

## 大结果 spool

当 kernel emit `large_result_spooled` observation 时，SDK 用 `LargeResultSpool` 持久化完整输出，并把 preview / ref 留在上下文中：

```python
from deepstrike.runtime.large_result_spool import LargeResultSpool

RuntimeOptions(
    ...,
    result_spool=LargeResultSpool(".spool", max_age_seconds=7 * 24 * 3600),
)
```

读取工具如果参数里包含 `.spool/...` 路径，`LocalExecutionPlane` 会尝试自动读取 spooled result。

## Kernel / Host 边界

| 行为 | 所属 |
|------|------|
| tool schema 是否暴露 | kernel + SDK 能力门控 |
| tool call 是否允许 | kernel syscall / governance |
| Python 函数调用 | SDK ExecutionPlane |
| subprocess / HTTP / 文件写入 | SDK / 工具 |
| 大结果是否需要 spool | kernel 决策，SDK 落盘 |
| worktree 生命周期 | SDK |

## 验证入口

- `python/tests/test_streaming_tools.py`
- `python/tests/test_tool_argument_repair.py`
- `python/tests/test_large_result_spool.py`
- `python/tests/test_worktree_isolation.py`
- `node/tests/remote-vpc-plane.test.ts`
