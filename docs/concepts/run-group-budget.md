# RunGroup 预算

`RunGroup` 是宿主 SDK 层的治理域：它让多个 stateless run / persona session 共享 **累计 token、累计 sub-agent spawn 和 lineage**。kernel 仍然是纯状态机；跨 run 的持久 ledger 存在 `GroupBudgetStore` 里。

主要实现入口：

- `python/deepstrike/runtime/run_group.py`
- `node/src/runtime/run-group.ts`
- `python/deepstrike/runtime/runner.py`
- `node/src/runtime/runner.ts`
- `crates/deepstrike-core/src/scheduler/state_machine/gate.rs`
- `crates/deepstrike-core/src/governance/quota.rs`

## 为什么 RunGroup 在 SDK 里

DeepStrike 的 kernel 可以被 stateless request handler 反复创建和销毁。如果“一个逻辑任务”由多个 session / persona / workflow bootstrap 共同完成，那么累计预算不能存在某个 kernel 实例里。

因此实现分成两层：

| 层 | 职责 |
|----|------|
| SDK `RunGroup` | 保存 group id、原子预留或累计记账、登记成员 |
| Kernel seed | 启动时接收 `group_tokens_base` / `group_spawns_base` |
| Kernel gate | 在本次 vehicle 内检查 token cap、spawn quota、workflow growth |
| SDK settle | run 结束时把 reservation 结算为实际消耗并释放剩余容量 |

## 数据模型

Python:

```python
@dataclass
class GroupLedger:
    tokens_spent: int = 0
    subagents_spawned: int = 0

@dataclass
class GroupMember:
    session_id: str
    role: str | None = None

@dataclass
class RunGroup:
    id: str
    budget_store: GroupBudgetStore
```

Node 对应字段是 camelCase：

```ts
interface GroupLedger {
  tokensSpent: number
  subagentsSpawned: number
}

interface RunGroup {
  id: string
  budgetStore: GroupBudgetStore
}
```

## 生命周期：reserve → seed → run → settle

```text
runner.run(session_id)
        │
        ├─ scope = GroupBudgetScope.open(...)
        ├─ atomic store: reserve(requested capacity)
        ├─ legacy store: read(accounting ledger)
        ├─ kernel configure_run:
        │    group_tokens_base = scope.ledger.tokens_spent
        │    group_spawns_base = scope.ledger.subagents_spawned
        │
        ▼
kernel run
        │
        ├─ SchedulerBudget.max_total_tokens checks:
        │    group_tokens_base + local total_tokens
        ├─ ResourceQuota.max_total_subagents checks:
        │    group_spawns_base + local_subagents_spawned()
        │
        ▼
run terminal
        │
        └─ scope.settle(tokens=total_tokens, subagents=spawned)
```

支持 reservation 的 store 会把先启动 member 的在途预留计入后续 member 的 seed，因此并发
member 不会针对同一份余额重复准入。只实现 `read/charge` 的 store 仍可累计记账，但不承诺并发配额。

## 累计预算 vs 瞬时预算

RunGroup 只共享 **累计** 预算，不共享瞬时并发状态。

| 预算 | 是否跨 RunGroup 累计 | 原因 |
|------|----------------------|------|
| `SchedulerBudget.max_total_tokens` | 是 | SDK seed `group_tokens_base`，kernel 按累计 token 判断 |
| `ResourceQuota.max_total_subagents` | 是 | SDK seed `group_spawns_base`，kernel 按累计 spawn 判断 |
| `ResourceQuota.max_concurrent_subagents` | 否 | running child 只存在当前 vehicle 的 `TaskTable` |
| `ResourceQuota.max_spawn_depth` | 否 | 当前 spawn lineage 的结构约束 |
| `memory_writes_per_window` | 否 | 当前 kernel 内的 observed clock / write timestamps |
| `max_workflow_nodes` | 当前 workflow | 根据 active `WorkflowRun` 节点数判断 |

这个边界很重要：跨进程的“已经花了多少”可以靠 ledger fold；跨进程的“此刻有多少正在跑”需要外部调度系统，不是 kernel 的事实源。

## Standalone Workflow

`RuntimeRunner.run_workflow(...)` 在没有 active parent run 时会 bootstrap 一个 workflow kernel。绑定 RunGroup 时，它会：

1. 把 standalone workflow session 登记为 group member。
2. 用 group ledger seed kernel。
3. workflow 完成后，把 envelope kernel 的 `local_subagents_spawned()` 作为 subagents charge 回 group。

这里每个 workflow node 都计入 spawn axis，因为 kernel 的 `TaskTable` 里每个 scheduled node 都是一个 child proc。

## ReactiveSession

`ReactiveSession` 把多个 persona runner 放进同一个逻辑会话。它要求所有 peer runner 使用同一个 `RunGroup`：

```python
session = ReactiveSession(
    run_group=group,
    make_runner=lambda pid, shared: RuntimeRunner(RuntimeOptions(
        ...,
        run_group=shared["run_group"],
    )),
    ...
)
```

成员关系通过 `GroupMember(session_id, role)` 记录，所以 lineage 可以跨 persona / invocation 查询。

## Budget Store

| 实现 | 位置 | 适用场景 |
|------|------|----------|
| `InMemoryGroupBudgetStore` | Python / Node | 单进程原子 reservation、开发与测试 |
| `SessionLogGroupBudgetStore` | Python / Node | 持久化 accounting 与 lineage；不提供并发 quota enforcement |
| 自定义 `ReservableGroupBudgetStore` | Redis / PostgreSQL 等 | 用事务或脚本实现跨副本 `reserve/settle/release` |

`SessionLogGroupBudgetStore` 的持久化方式是 fold event：

| event kind | 用途 |
|------------|------|
| `group_budget_charged` | 累加 tokens / subagents |
| `group_member_joined` | 登记 session membership，按 session_id 幂等 |

由于通用 `SessionLog` 没有 CAS/事务接口，SDK 不再把 event fold 描述成跨副本预算强制。跨副本部署
必须提供 reservable store；不能用进程内 mutex 包装日志来伪造这个保证。

## 配置示例

```python
from deepstrike import (
    InMemoryGroupBudgetStore,
    ResourceQuota,
    RunGroup,
    RuntimeOptions,
    RuntimeRunner,
)

store = InMemoryGroupBudgetStore()
group = RunGroup(id="incident-42", budget_store=store)

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    run_group=group,
    max_total_tokens=100_000,
    resource_quota=ResourceQuota(max_total_subagents=12),
))
```

多个 runner 只要传入同一个 `RunGroup.id` 与共享 store，就会共享累计治理域。

## 常见误解

| 误解 | 实际实现 |
|------|----------|
| kernel 持久化 RunGroup ledger | ledger 在 SDK `GroupBudgetStore`，kernel 只接收 seed |
| `max_concurrent_subagents` 跨所有 member 生效 | 它只看当前 kernel 的 running child tasks |
| completed sub-agent 会释放 `max_total_subagents` | 不会；total 是累计轴 |
| standalone workflow 不计入 group spawn | 会计入；完成时按 workflow node count charge |
| 不绑定 RunGroup 时预算坏掉 | 不会；回退到 N=1 的 per-run 预算 |

## 验证入口

- `python/tests/test_run_group_budget.py`
- `node/tests/run-group-budget.test.ts`
- `python/tests/test_workflow_drive.py`
- `node/tests/e2e/composition.test.ts`

## 延伸阅读

- [Signals 与 Reactive](../guides/signals-and-reactive)
- [Governance](../guides/governance)
- [动态工作流](../guides/workflow)
