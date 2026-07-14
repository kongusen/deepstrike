# Governance

Governance 是 Agent OS 的 **Syscall Governance Plane**。它在工具执行、memory 写入、workflow 增长和 sub-agent spawn 之前裁决权限、配额与参数约束；拒绝不是事后日志，而是会写回 context 的 rollback note。

**代码**：`crates/deepstrike-core/src/governance/`、`python/deepstrike/governance.py`

---

## 在 Agent OS 中的位置

| 裁决点 | 说明 |
|--------|------|
| Tool syscall | 工具 schema 暴露和实际调用前都可被过滤或拒绝 |
| Workflow syscall | `SubmitNodes` / `LoadWorkflow` 受节点数、深度和资源配额限制 |
| Memory syscall | 写入频率、内容大小和 metadata 由 policy 控制 |
| Process spawn | sub-agent 并发、总数、隔离模式可被拦截 |
| Context feedback | deny / ask_user 结果作为 rollback note 进入下一轮上下文 |

治理面的目标是让 agent 的每个外部动作都像 OS syscall 一样可解释、可拒绝、可追踪，而不是把风险留给工具函数自己处理。

![Syscall Governance Funnel](/governance_pipeline.svg)

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
- `LoadWorkflow { node_count }` — bootstrap / flatten DAG

超 `max_workflow_nodes` 时 deny + rollback note，workflow 继续但拒绝增长。

---

## I5：Schema 预过滤

`GovernancePolicy.surface_denied_in_system=True`（默认）时，runner 预过滤 denied 工具，并在 system 中 surface 拒绝列表。

## 有状态决策 hook

`on_tool_call` 是执行前决策边界。hook 抛错时默认 fail-closed，tool call 会以
`governance_denied` 返回；只有纯 advisory hook 才应显式设置
`on_tool_call_failure="open"`。`on_tool_result` 发生在工具副作用之后，仍按 observer/enrichment
语义隔离失败，不能反向声称工具未执行。

---

## 延伸阅读

- [Sub-Agent 与协作](./sub-agents-and-collaboration) — sandbox / isolation
- [执行平面与工具](./execution-plane-and-tools) — 工具实际执行位置与审计回调
- [OS Profile 与运行时快照](./os-profile-and-snapshots) — profile、policy 与 dashboard 状态
- 测试：`python/tests/test_resource_quota.py`
