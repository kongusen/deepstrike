# Agent 运行策略与状态快照

运行策略决定 Agent 默认如何处理 Signal、权限和资源。状态快照则把 SessionLog 中的事件整理成可观测的运行摘要，供 dashboard、调试与运维使用。`OsProfile` 和 `OS Snapshot` 是现有 API 名称，分别代表这两类能力。

**代码入口**：

- `python/deepstrike/runtime/os_profile.py`
- `python/deepstrike/runtime/os_snapshot.py`
- `node/src/runtime/os-profile.ts`
- `node/src/runtime/os-snapshot.ts`
- `node/src/runtime/kernel-primitives-dashboard.ts`

## 应用可以观察和配置什么

| 职责 | 说明 |
|------|------|
| Profile | 把 attention、governance 等默认策略打包成应用可选择的运行配置 |
| Validation | 在启动前校验 declarative policy，避免无效配置进入 runtime |
| Snapshot | 从 SessionLog 折叠运行状态，而不是依赖内存中的瞬时对象 |
| Dashboard | 将运行时事件转成前端可消费的健康、队列、权限和进程状态 |

`OsProfile` 回答“Agent 用什么默认策略启动”，`OS Snapshot` 回答“这次运行发展到了什么状态”。前者设置边界，后者支撑观测。

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

它提供的是可运行的基础默认，不是生产安全策略。

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
| `tool_gated_count` | `tool_gated` |
| memory counters | `memory_*` events |

## Level 5：检查事件分类完整性

```python
from deepstrike.runtime.os_snapshot import session_log_has_required_categories

events = [entry.event for entry in await session_log.read("session-1")]
assert session_log_has_required_categories(events)
```

这会检查运行时事件是否带有正确的 `category` 和 `primitive`，适合 CI 或 dashboard ingest 前校验。

## 与可恢复 Checkpoint 的区别

| 名称 | 用途 | 是否可恢复执行 |
|------|------|----------------|
| OS Snapshot | 从 SessionLog 折叠出的观测摘要 | 否 |
| Kernel Checkpoint | opaque logical state、digest 与 bounded journal tail | 是，服务精确 wake / replay |
| ContextSnapshot | Context 分区快照 | 部分，服务 context restore |

`OS Snapshot` 面向人和监控系统，`Kernel Checkpoint` 面向运行恢复。checkpoint 不序列化私有 state-machine struct，也不保存完整 accepted-input 历史或派生的 planned step；它按 transition/P1/P2/P3 owner 保存 logical state，并用 state/tail digest 校验。应用通过 candidate -> 持久化 -> covered-head CAS install -> ack 协议管理 checkpoint，恢复时只回放 bounded tail 和 checkpoint 之后的 journal records。

## 生产建议

1. 不要直接用 `native` profile 当生产安全边界。
2. 把 write / deploy / shell 类工具默认设为 `ask_user` 或 `deny`。
3. 给 dashboard ingest 增加 `session_log_has_required_categories` 检查。
4. 对长期 run 定期构建 OS Snapshot，观察 `tool_gated_count`、paging 和 memory validation failure。
5. Profile 与 `ResourceQuota` 配合使用；profile 管策略，quota 管资源。
6. 按恢复目标配置 checkpoint tail 的 count/byte soft watermark 与 hard limit；hard limit 返回可重试的 `CheckpointRequired`，不会接受该 input，也不会设置永久 overflow latch。

## 验证入口

- `python/tests/test_os_profile.py`
- `python/tests/test_os_snapshot.py`
- `node/tests/os-profile.test.ts`
- `node/tests/kernel-primitives-dashboard.test.ts`
