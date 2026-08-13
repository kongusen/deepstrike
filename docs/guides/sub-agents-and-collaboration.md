# Sub-Agent 与协作

Sub-Agent 让一个 Agent 把专注职责委托给另一个 Agent。在 [Agent Process Runtime](../architecture/agent-process-runtime) 中，这不是匿名地多开几个模型调用，而是创建一个具有父子谱系、收窄权限、预留预算、可等待和受监督的子任务。角色、Context 继承、工具访问、隔离、contract 和 handoff 共同定义它的公开行为。

**代码**：
- `python/deepstrike/types/agent.py` — `AgentRunSpec`
- `python/deepstrike/collaboration/` — `AgentPool`、`ContractDrivenHarness`
- Runtime：`crates/deepstrike-core/src/proc/`、`scheduler/state_machine/process.rs`

---

## 可以控制子 Agent 的什么

| 职责 | 说明 |
|------|------|
| 进程身份 | `AgentRunSpec.identity` 和 parent-child lineage 写入 session log |
| 角色边界 | explore / plan / implement / verify 决定默认 prompt、工具和上下文继承 |
| 隔离边界 | shared / read_only / worktree / remote 映射到不同执行面和 cwd 策略 |
| 能力边界 | `capability_filter` 与 Skill / Governance 一起控制工具可见性 |
| 交接边界 | Contract 与 HandoffArtifact 把子进程产物变成父进程可消费的证据 |

这样多 Agent 工作就能被追踪、限制，并和工作流、评估自然组合。

## Runtime 保证

| 子 Agent 动作 | Agent Process Runtime 语义 |
| --- | --- |
| Spawn | 父节点来自实际 caller；能力只能衰减；预算先预留再启动 |
| Wait / Join | 父任务进入持久 WaitSet，不占用 runnable 槽位 |
| Communicate | Mailbox、Channel 和对象 handle 仍受 capability 检查 |
| Fail | 按 Propagate、Isolate、Restart、Retry 或 Ignore 策略处理并记录监督事件 |
| Recover | 谱系、等待进度、预算、IPC 与监督状态随 checkpoint 恢复 |

![Process Isolation & Sub-Agents Mechanisms](/collaboration_mechanisms.svg)

## 概念

### AgentRunSpec 关键字段

| 字段 | 说明 |
|------|------|
| `role` | explore / plan / implement / verify / custom |
| `isolation` | shared / read_only / worktree / remote |
| `context_inheritance` | none / system_only / full |
| `capability_filter` | 允许的工具 kind / id |
| `milestones` | 分阶段验收契约 |

### 隔离模式

| isolation | 行为 |
|-----------|------|
| `shared` | 共享父 context（默认） |
| `read_only` | 只读继承，适合 explore |
| `worktree` | Git worktree 隔离 cwd |
| `remote` | 远程 VPC / 沙箱 plane |

---

## Level 1：Workflow 节点即 Sub-Agent

每个 `WorkflowNodeSpec` spawn 一个隔离 sub-agent — 见 [动态工作流](./workflow)。

```python
WorkflowNodeSpec(
    task="安全审计",
    role="verify",
    isolation="read_only",
    context_inheritance="system_only",
)
```

---

## Level 2：AgentPool 角色分工

```python
from deepstrike import AgentPool

pool = AgentPool()
pool.add("orchestrator", orchestrator_runner)
pool.add("executor", executor_runner)
pool.add("verifier", verifier_runner)
pool.configure_coordinator(orchestrator_runner.host_options, session_id="collab-1")

result = await pool.spawn(
    role="executor",
    goal="实现功能 X",
    parent_session_id="collab-1",
)
```

`configure_coordinator` 启用 kernel spawn path，parent-child lineage 写入 session log。

---

## Level 3：Verification Contract

```python
from deepstrike import (
    ContractBuilder, ContractDrivenHarness,
    AcceptanceCriterion, format_contract_for_system_prompt,
)

contract = ContractBuilder("feature-x").add_criteria([
    AcceptanceCriterion(id="tests", text="All unit tests pass", required=True),
]).build()

harness = ContractDrivenHarness(runner, contract, ...)
outcome = await harness.run(goal="Implement feature X")
```

Creator-Verifier 分离，缓解 self-preferential bias。

---

## Level 4：Handoff

```python
from deepstrike import HandoffBus, HandoffArtifact

bus = HandoffBus()
await bus.publish(HandoffArtifact(
    from_agent="executor",
    to_agent="verifier",
    content="Implementation complete. See diff in ...",
))
```

Handoff 产物进入 knowledge 分区供下游 agent 消费。

---

## SubAgentHarnessConfig

子 agent 自动走质量门控重试：

```python
from deepstrike import SubAgentHarnessConfig

RuntimeOptions(
    ...,
    sub_agent_harness=SubAgentHarnessConfig(
        eval_provider=judge_provider,
        max_attempts=3,
    ),
)
```

---

## 延伸阅读

- [Agent Process Runtime](../architecture/agent-process-runtime)
- [Harness 与 Eval](./harness-and-eval)
- [Milestones](./milestones)
- [角色与隔离](../concepts/roles-and-isolation)
