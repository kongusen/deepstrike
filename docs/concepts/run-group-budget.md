# RunGroup 预算

`RunGroup` 是宿主 SDK 的跨 run 治理域：多个 stateless run、persona session 和 standalone workflow 可以共享累计 token、sub-agent spawn、loop round 与 lineage。跨 run 状态由 `GroupBudgetStore` 持有；kernel 只持有本次 vehicle 获批的 reservation grant。

## 唯一预算协议

加入 RunGroup 的执行只使用一条事务路径：

```text
join member
    │
    ▼
reserve(limits, requested) ──► reservation_id + granted capacity
    │
    ▼
configure_run.budget_grant
    │
    ▼
kernel 本地执行与强制限制
    │
    ▼
budget_usage_reported(reservation_id, actual usage)
    │
    ▼
settle(reservation_id, actual) / 失败前 release(reservation_id)
```

旧的 `group_tokens_base`、`group_spawns_base`、`group_rounds_base`、`read/charge` host fallback 和 `SessionLogGroupBudgetStore` 已删除。SDK 不会在终态读取 `LoopResult` 或 `local_subagents_spawned()` 猜测结算值。

## 职责边界

| 组件 | 职责 |
|------|------|
| `GroupBudgetStore` | 原子计算 settled + held、预留容量、幂等 settle/release、维护成员 lineage |
| SDK runner | 在 `start_run` 前 reserve；把 grant 交给 kernel；按相关 usage report 结算 |
| Kernel | 强制本次 grant；为 exceeded/usage 事件附带 operation 与 reservation identity |

`GroupBudgetStore` 必须实现 `join`、`members`、`reserve`、`settle`、`release`。跨副本实现应使用 Redis script、数据库事务或等价的原子机制。通用 append-only SessionLog 没有 CAS，因此不能作为预算预留实现。

## 预算轴

| 轴 | RunGroup 行为 |
|----|---------------|
| `max_total_tokens` | reserve token capacity；kernel 以 grant 作为本 run 的有效累计上限 |
| `max_total_subagents` | reserve spawn capacity；普通 spawn 与 workflow node 走同一个 kernel gate |
| loop `max_rounds` | 每轮只申请 1 round，避免多个 loop vehicle 并发超卖 |
| `max_concurrent_subagents` | 仍是当前 kernel 的瞬时限制，不跨 vehicle |
| `max_spawn_depth` | 当前 lineage 的结构限制，不跨 vehicle |
| `memory_writes_per_window` | 当前 kernel observed clock 的速率限制 |

未申请的轴在 `budget_grant` 中省略，表示本次 admission 不限制该轴；它不是零容量。kernel 的 usage report 仍可记录该轴实际用量。

## Standalone Workflow

`run_workflow()` 没有 active parent 时会创建一个真实 kernel run。SDK 先 reserve，再启动 workflow；DAG 完成后发送 `complete_run`，由 kernel 产生普通 `done` 和一次 correlated `budget_usage_reported`。因此 workflow node 数来自 kernel TaskTable，而不是宿主旁路计数。

## 示例

```python
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

`InMemoryGroupBudgetStore` 适合单进程与测试。生产环境应提供实现同一 reservation contract 的 durable store。

验证入口：`python/tests/test_run_group_budget.py`、`node/tests/run-group-budget.test.ts`、`python/tests/test_workflow_drive.py`。
