# Governance

治理为 Agent 设定清晰边界。你可以允许、拒绝或暂停工具调用，约束参数，限制委托和 Memory 写入，并把审批变成运行过程的一部分。

**代码**：`crates/deepstrike-core/src/governance/`、`python/deepstrike/governance.py`

---

## 应用可以做出的决定

| 决定 | 示例 |
| --- | --- |
| 允许或拒绝 | 只让 Agent 起草内容，不让它看到 `publish_public`。 |
| 请求审批 | `email_editor` 在真人批准前暂停。 |
| 约束参数 | 要求 path、enum 或数值范围。 |
| 限制资源 | 限制 turn、token、子 Agent、workflow node 或 Memory 写入。 |
| 安全取消 | 应用不再需要时停止 run 或子 Agent。 |

治理决策会呈现给 Agent 并记录在 Session 中，模型可以调整行为，应用也可以解释发生了什么。

![Agent 治理决策流程](/governance_pipeline.svg)

## 概念

| 机制 | 说明 |
|------|------|
| Permission | allow / deny / ask_user |
| Veto | 硬禁工具列表 |
| Rate limit | 滑动窗口调用上限 |
| Constraint | 参数 required / enum / range |
| ResourceQuota | subagent 并发、深度、memory write 频率 |
| Sandbox | 子 agent 隔离 profile |

---

## Level 1：声明式策略

```python
from deepstrike import GovernancePolicy, GovernancePolicyRule, GovernanceRateLimit

policy = GovernancePolicy(
    default_action="ask_user",
    rules=[
        GovernancePolicyRule(pattern="write_*", action="deny"),
        GovernancePolicyRule(pattern="read_*", action="allow"),
    ],
    vetoes=["dangerous_tool"],
    rate_limits=[
        GovernanceRateLimit(tool="search", max_calls=10, window_ms=60_000),
    ],
)

RuntimeOptions(..., governance_policy=policy)
```

`ask_user` 时 emit `PermissionRequestEvent`，需 `on_permission_request` 回调解析。

---

## Level 2：参数约束

```python
policy = GovernancePolicy(
    constraints=[
        {"kind": "required", "tool": "write_file", "path": "path"},
        {"kind": "enum", "tool": "set_mode", "path": "mode", "values": ["read", "write"]},
        {"kind": "range", "tool": "resize", "path": "size", "min": 1, "max": 1000},
    ],
)
```

---

## Level 3：ResourceQuota

```python
from deepstrike import ResourceQuota, MemoryWriteRateLimit

RuntimeOptions(
    ...,
    resource_quota=ResourceQuota(
        max_concurrent_subagents=3,
        max_total_subagents=20,
        max_spawn_depth=2,
        memory_writes_per_window=MemoryWriteRateLimit(max_writes=5, window_ms=60_000),
    ),
)
```

配合 `RunGroup` 可跨多次 stateless run 累计 spawn 计数 — 见 [RunGroup 预算](../concepts/run-group-budget)。

---

## Level 4：Syscall trap

Workflow 增长走内核 syscall：

- `SubmitNodes { count }` — append 节点
- `AppendWorkflowNodes { nodes }` — 在 kernel 派生 caller 后扩展 DAG

超 `max_workflow_nodes` 时，kernel 提交 `control_request_rejected` observation；请求从未执行，
因此不会 rollback。顶层 `run_workflow` 通过 `WorkflowOutcome.rejection` 返回原因；运行中
的节点提交被拒时，该提交者节点以失败 outcome 结束，root agent 可直接看到真实结果。

---

## I5：Schema 预过滤

`GovernancePolicy.surface_denied_in_system=True`（默认）时，runner 预过滤 denied 工具，并在 system 中 surface 拒绝列表。

## 有状态决策 hook

`on_tool_call` 是执行前决策边界。hook 抛错时默认 fail-closed，tool call 会以
`governance_denied` 返回；只有纯 advisory hook 才应显式设置
`on_tool_call` 异常时始终拒绝执行。`on_tool_result` 发生在工具副作用之后，仍按 observer/enrichment
语义隔离失败，不能反向声称工具未执行。

---

## 延伸阅读

- [Sub-Agent 与协作](./sub-agents-and-collaboration) — sandbox / isolation
- [执行平面与工具](./execution-plane-and-tools) — 工具实际执行位置与审计回调
- [OS Profile 与运行时快照](./os-profile-and-snapshots) — profile、policy 与 dashboard 状态
- 测试：`python/tests/test_resource_quota.py`
