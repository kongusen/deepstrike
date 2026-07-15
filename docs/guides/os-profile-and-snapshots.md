# OS Profile 与运行时快照

OS Profile 是一组宿主可选择的默认治理策略：attention queue、governance policy、native profile 校验。OS Snapshot 则是从 SessionLog 折叠出的运行时状态摘要，用于 dashboard、debug 和运维观测。

**代码入口**：

- `python/deepstrike/runtime/os_profile.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `node/src/runtime/os-profile.ts`
- `node/src/runtime/os-snapshot.ts`
- `node/src/runtime/kernel-primitives-dashboard.ts`

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| Profile | 把 attention、governance 等默认策略打包成宿主可选择的运行配置 |
| Validation | 在启动前校验 declarative policy，避免无效配置进入 kernel |
| Snapshot | 从 SessionLog 折叠运行时状态，而不是依赖内存中的瞬时对象 |
| Dashboard | 将 kernel primitives 转成前端可消费的健康、队列、权限和进程状态 |

OS Profile 是“以什么策略启动 Agent OS”，OS Snapshot 是“当前 Agent OS 跑成了什么状态”。前者配置边界，后者支撑观测。

![OS Profile & Snapshots Mechanisms](/snapshots_mechanisms.svg)

## Level 1：使用 native profile

```python
from deepstrike import RuntimeOptions, RuntimeRunner, os_profile

profile = os_profile("native")

runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
    os_profile=profile,
))
```

`native` profile 默认：

| 策略 | 默认 |
|------|------|
| SignalPolicy | `queue_max=64` |
| GovernancePolicy | `pattern="*" action="allow"` |

它提供的是“内核原语可用”的基础默认，不是生产安全策略。

## Level 2：校验 profile

```python
from deepstrike import assert_native_profile

profile = assert_native_profile("native")
```

`validate_declarative_policy` 会检查：

- governance rules 必须是 list
- rule pattern 必须是 string
- action 只能是 `allow` / `deny` / `ask_user`
- signal `queue_max` 必须是正整数；可选 `ttl_ms` 也必须为正整数

## Level 3：自定义 OsProfile

```python
from deepstrike import GovernancePolicy, GovernancePolicyRule, OsProfile
from deepstrike.runtime.os_profile import SignalPolicy

profile = OsProfile(
    id="review-safe",
    signal_policy=SignalPolicy(queue_max=32, ttl_ms=60_000),
    governance_policy=GovernancePolicy(
        default_action="ask_user",
        rules=[
            GovernancePolicyRule(pattern="read_*", action="allow"),
            GovernancePolicyRule(pattern="write_*", action="ask_user"),
            GovernancePolicyRule(pattern="run_*", action="deny"),
        ],
    ),
)
```

把 profile 传入 `RuntimeOptions(os_profile=profile)` 后，SDK 会把它 lower 到 kernel config。

## Level 4：OS Snapshot

从 SessionLog 事件构建运行时摘要：

```python
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events

events = [entry.event for entry in await session_log.read("session-1")]
snapshot = rebuild_os_snapshot_from_session_events(events)

print(snapshot.last_suspend)
print(snapshot.process_by_agent)
print(snapshot.budget_exceeded)
```

Snapshot 统计：

| 字段 | 来源事件 |
|------|----------|
| `last_suspend` | `suspended` |
| `last_resumed_turn` | `resumed` |
| `process_by_agent` | `agent_process_changed` |
| `budget_exceeded` | `budget_exceeded` |
| `signals` | `signal_delivery_disposed` |
| `page_out_count` / `page_in_count` | memory paging |
| `spool_count` | `large_result_spooled` |
| `tool_gated_count` | `tool_gated` |
| memory counters | `memory_*` events |

## Level 5：检查事件分类完整性

```python
from deepstrike.runtime.os_snapshot import session_log_has_required_categories

events = [entry.event for entry in await session_log.read("session-1")]
assert session_log_has_required_categories(events)
```

这会检查 kernel event 是否带有正确的 `category` 和 `primitive`，适合 CI 或 dashboard ingest 前校验。

## 与 Kernel Snapshot 的区别

| 名称 | 用途 | 是否可恢复执行 |
|------|------|----------------|
| OS Snapshot | 从 SessionLog 折叠出的观测摘要 | 否 |
| KernelSnapshotV2 | 已接受 ABI 事务与校验元数据 | 是，服务精确 wake / replay |
| ContextSnapshot | Context 分区快照 | 部分，服务 context restore |

OS Snapshot 面向人和监控系统；`KernelSnapshotV2` 面向 runtime 恢复。后者不序列化私有 state-machine struct，而是确定性重放 public ABI，并核对 lifecycle、operation、step/effect identity 与 terminal latch。Node 使用 `snapshotKernelRuntime` / `restoreKernelRuntime`，Python 使用 `snapshot_kernel_runtime` / `restore_kernel_runtime`。`kernelReliability.snapshotInputLimit` / `KernelReliability.snapshot_input_limit` 控制可恢复事务上限。

## 生产建议

1. 不要直接用 `native` profile 当生产安全边界。
2. 把 write / deploy / shell 类工具默认设为 `ask_user` 或 `deny`。
3. 给 dashboard ingest 增加 `session_log_has_required_categories` 检查。
4. 对长期 run 定期构建 OS Snapshot，观察 `tool_gated_count`、`spool_count`、memory validation failure。
5. Profile 与 `ResourceQuota` 配合使用；profile 管策略，quota 管资源。
6. 按故障恢复窗口设置 snapshot input limit；达到上限会显式返回 `snapshot_incompatible`，不会生成不完整快照。

## 验证入口

- `python/tests/test_os_profile.py`
- `python/tests/test_os_snapshot.py`
- `node/tests/os-profile.test.ts`
- `node/tests/kernel-primitives-dashboard.test.ts`
