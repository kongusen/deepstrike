# 角色与隔离

DeepStrike 不把 sub-agent 隔离当成 prompt 约定，而是把它降成内核可检查的执行契约。相关事实源主要在：

- `crates/deepstrike-core/src/types/agent.rs`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `crates/deepstrike-core/src/scheduler/tcb.rs`
- `crates/deepstrike-core/src/proc/mod.rs`
- `python/deepstrike/types/agent.py`

## 核心模型

一次 spawn 从 host 侧的 `AgentRunSpec` 进入 kernel 后，会变成 `IsolationManifest`，再落到 `Tcb.proc`，最后按需投影成 `AgentProcess` observation / SDK ABI。

```text
AgentRunSpec
  role + isolation + capability_filter
        │
        ▼
IsolationManifest::from_spec(...)
  agent_id · parent_session_id · role · isolation
  context_inheritance · permitted_capability_ids
        │
        ▼
Tcb { proc: Some(ProcInfo), caps, budget, state }
        │
        ▼
AgentProcess::from_tcb(...)
```

这里有一个容易误读的点：普通 `AgentRunSpec` 不直接传 `context_inheritance` 字段给 kernel；`IsolationManifest::from_spec` 会按 `role` 推导默认继承策略。Workflow node 则有自己的 `context_inheritance` 字段和 role default，用于 DAG 模板。

## Role

Rust core 的枚举是 `AgentRole`，Python / Node SDK 暴露为字符串字面量：

```python
KernelAgentRole = Literal["explore", "plan", "implement", "verify", "custom"]
```

| role | 语义 | 常见宿主行为 |
|------|------|--------------|
| `explore` | 读取、调研、搜索，偏信息收集 | 默认少继承上下文，常配 `read_only` |
| `plan` | 规划、合成、编排 | 需要看到较完整上下文 |
| `implement` | 修改代码或产出实现 | 常需要 `worktree` 或写权限 |
| `verify` | 验证、审计、裁判 | 应尽量避免继承作者上下文 |
| `custom` | 宿主自定义职责 | 默认最保守 |

## 默认继承策略

普通 spawn 的默认继承来自 `IsolationManifest::role_default_context_inheritance`：

| role | 默认 `ContextInheritance` |
|------|---------------------------|
| `explore` | `system_only` |
| `verify` | `system_only` |
| `plan` | `full` |
| `implement` | `full` |
| `custom` | `none` |

Workflow node 的默认来自 `role_defaults(role)`，更偏 workflow 安全边界：

| role | 默认 `AgentIsolation` | 默认 `ContextInheritance` |
|------|------------------------|---------------------------|
| `explore` | `read_only` | `system_only` |
| `verify` | `read_only` | `none` |
| `plan` | `shared` | `full` |
| `implement` | `worktree` | `full` |
| `custom` | `shared` | `none` |

这就是为什么 verifier 通常不继承被验证者上下文：它要降低 self-preferential bias，而不是复读作者的解释。

## Isolation

```python
AgentIsolation = Literal["shared", "read_only", "worktree", "remote"]
```

| isolation | 内核语义 | host 责任 |
|-----------|----------|-----------|
| `shared` | 可在父 run 的普通执行域内运行 | SDK 决定具体 cwd / 工具 plane |
| `read_only` | 只读语义；适合 untrusted explore / verify | ExecutionPlane 应不给写工具或写目录 |
| `worktree` | 需要独立工作目录 | Python `RuntimeOptions.worktree_manager` 创建和清理 git worktree |
| `remote` | 远程隔离执行 | 宿主接入 remote sandbox / VPC / process sandbox |

kernel 只持有声明式状态，不直接创建 worktree、远程沙箱或文件系统权限。真实 I/O 隔离由 SDK / ExecutionPlane 执行。

## Capability Filter

`AgentCapabilityFilter` 是 spawn 时的能力裁剪：

```python
@dataclass
class AgentCapabilityFilter:
    allowed_kinds: list[str] = field(default_factory=list)
    allowed_ids: list[str] = field(default_factory=list)
```

Rust core 用它过滤父任务当前的 `CapabilityManifest`，并把结果写入 `IsolationManifest.permitted_capability_ids` 与 child `Tcb.caps`。

| 字段 | 行为 |
|------|------|
| `allowed_kinds` 为空 | 不按 kind 限制 |
| `allowed_ids` 为空 | 不按 id 限制 |
| 两者都非空 | 必须同时满足 kind 与 id |

它会和这些机制叠加：

- `RuntimeOptions.allowed_tool_ids`：静态每 run 工具 profile
- Skill gating：已激活 skill 声明的工具 allow-set
- Governance：syscall trap 上的 allow / deny / gate
- ResourceQuota：spawn、memory write、workflow growth 等资源上限

## 子 agent 工具面三条路

spawn 出来的子 agent 拿到什么工具，由 host 侧的 `AgentRunSpec.tool_access`（Node 侧 `toolAccess`，仅 host 侧、不进 kernel）与 capability 挂载共同决定，一共三条路：

| 路径 | 配置 | 语义 |
|------|------|------|
| 继承父面 | `tool_access="inherit"` | 子直接跑在父的 execution plane 上，拿到父的工具与 meta-tool 可用面（与信任 workflow node 同机制）。子面 ≤ 父面，不提权。 |
| 精细授权 | 默认 `"filtered"` + capability 挂载 + `capability_filter` | 子只拿到 manifest 里被授予的能力。注意 `set_tools` 只填充 `sm.tools`、**不进 spawn manifest**；要让子能拿到某个工具，必须把它作为 capability 挂载（mount），再用 `capability_filter` 授予。 |
| 默认 deny-all | 默认 `"filtered"`，无挂载 / 无 filter | 子的工具面为空，模型会报"无工具可用"。此时 SDK 发一条宿主可见告警教你怎么修；若确实要一个无工具子代理，忽略即可。 |

## NodeTrust 与 quarantine

Workflow node 额外有 `NodeTrust`：

```python
NodeTrust = Literal["trusted", "quarantined"]
```

`quarantined` 表示该 node 读过不可信输入。实现上有三条内核约束：

| 约束 | 实现位置 | 行为 |
|------|----------|------|
| quarantine 必须只读 | `scheduler/state_machine/workflow.rs` | quarantined node 如果声明写能力 isolation，会在 spawn 时被 deny |
| taint 传递 | `orchestration/workflow/run.rs` | quarantined submitter 追加的节点会被强制改成 quarantined |
| 跨边界标记 | `scheduler/state_machine/process.rs` | quarantined child 输出回到 trusted parent 时会带 untrusted-origin 标记 |

这不是 prompt 建议，而是 DAG 拓扑层面的 no-privilege-escalation。

## TCB 与 AgentProcess

`AgentProcess` 不是第二份状态源。代码里 `AgentProcess::from_tcb` 从 child `Tcb` 重建进程视图：

| TCB 字段 | AgentProcess 字段 |
|----------|-------------------|
| `tcb.id` | `agent_id` |
| `tcb.proc.parent_session_id` | `parent_session_id` |
| `tcb.proc.role` | `role` |
| `tcb.proc.isolation` | `isolation` |
| `tcb.proc.context_inheritance` | `context_inheritance` |
| `tcb.caps` | `permitted_capability_ids` |
| `TaskState::Done(...)` | `joined` / `failed` |

因此 lineage、状态、预算、权限视图最终都回到 `TaskTable`。

## 常见误解

| 误解 | 实际实现 |
|------|----------|
| role 只是 prompt 里的身份 | role 参与默认 inheritance / isolation、workflow 模板和进程视图 |
| worktree 隔离由 kernel 创建 | kernel 只声明 `AgentIsolation::Worktree`；SDK 执行 |
| verifier 自动完全看不到父上下文 | 普通 spawn 和 workflow node 默认不同；workflow verify 默认 `none`，普通 spawn 默认 `system_only` |
| quarantined 是文档标签 | kernel 会 deny 写能力、传递 taint、标记跨边界输出 |

## 延伸阅读

- [Sub-Agent 与协作](../guides/sub-agents-and-collaboration)
- [Governance](../guides/governance)
- [WorkflowNodeSpec](../reference/workflow-node-spec)
