# Session、Replay 与恢复

SessionLog 是 DeepStrike 的证据链：每个 run 的 LLM 输出、工具请求、工具结果、压缩、权限、进程、memory、workflow 事件都会 append 到同一个 session stream。它支撑三件事：

- **恢复**：从 session events 重建状态或 workflow 进度
- **审计**：按 kernel primitive 过滤关键事件
- **复现**：用 provider replay / ReplayProvider 重放模型输出

**代码入口**：

- `python/deepstrike/runtime/session_log.py`
- `python/deepstrike/runtime/session_repair.py`
- `python/deepstrike/runtime/provider_replay.py`
- `python/deepstrike/runtime/replay_provider.py`
- `python/deepstrike/runtime/replay_fixture.py`
- `python/deepstrike/runtime/os_snapshot.py`

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 事件日志 | SessionLog 是 run 的 append-only evidence stream |
| 恢复 | workflow、memory、permission、tool、provider 事件可折叠成当前状态 |
| 审计 | 可按 kernel primitive 过滤关键事件，定位哪个平面发生了什么 |
| 复现 | provider replay / ReplayProvider 让测试不依赖真实模型调用 |
| 运维 | OS Snapshot 从 session events 汇总 dashboard 所需状态 |

Session 面相当于 Agent OS 的 journal：没有它，系统只能“跑完一次”；有了它，才能解释、恢复、重放和运营。

![Session Replay & Recovery Mechanisms](/session_replay_mechanisms.svg)

## Level 1：选择 SessionLog

开发内存日志：

```python
from deepstrike import InMemorySessionLog

session_log = InMemorySessionLog()
```

本地持久 JSONL：

```python
from deepstrike import FileSessionLog

session_log = FileSessionLog("./sessions")
```

`FileSessionLog` 是单 instance 顺序 append；多进程写同一 session 需要外部锁或数据库实现 `SessionLog` 协议。

## Level 2：固定 session_id

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=session_log,
))

async for event in runner.run("修复支付 bug", session_id="pay-bug-42"):
    ...

events = await session_log.read("pay-bug-42")
```

不传 `session_id` 时 SDK 会生成新 id；想恢复 / 审计 / 对齐 RunGroup membership 时应显式传入。

## Level 3：读事件与过滤

```python
events = await session_log.read("pay-bug-42")
latest = await session_log.latest_seq("pay-bug-42")

# 只看某个 kernel primitive
memory_events = await session_log.read(
    "pay-bug-42",
    primitive_filter="memory",
)
```

常见事件：

| kind | 用途 |
|------|------|
| `run_started` / `run_terminal` | run 生命周期 |
| `llm_completed` | assistant 文本、tool_calls、provider_replay |
| `tool_requested` / `tool_completed` | 工具证据 |
| `compressed` / `context_renewed` | Context VM 压缩与 renewal |
| `tool_gated` / `permission_requested` / `permission_resolved` | 权限路径 |
| `agent_process_changed` | sub-agent lineage |
| `workflow_node_completed` / `workflow_nodes_submitted` | workflow 恢复 |
| `memory_written` / `memory_queried` / `memory_validation_failed` | memory syscall |

## Level 4：Provider replay

Provider replay 解决的问题是：某些 provider 的 assistant message 不是纯文本，可能包含 native blocks、reasoning details 或 stateful response id。SDK 会把可重放 envelope 存到 `llm_completed.provider_replay`。

```python
from deepstrike.runtime.provider_replay import seed_provider_replay_from_events

events = await session_log.read("pay-bug-42")
seed_provider_replay_from_events(provider, events)
```

重放兼容性按 provider descriptor 检查：

- protocol 相同 → 可 seed
- protocol 不同 → 跳过该 replay envelope，使用 neutral transcript
- provider 没有 replay API → no-op

## Level 5：ReplayProvider 离线测试

从 session events 抽取 assistant messages：

```python
from deepstrike import ReplayProvider, ReplayProviderOpts, extract_recorded_messages

events = await session_log.read("pay-bug-42")
messages = extract_recorded_messages(events)
provider = ReplayProvider(ReplayProviderOpts(messages=messages))
```

然后把 `provider` 传给 `RuntimeRunner`，即可离线跑固定 assistant 输出的测试。适合回归测试 runtime 行为、工具执行、governance 和 workflow 驱动。

## Level 6：修复坏事件

如果旧日志缺少 `token_count` 或包含过大的 replay 文本，可先 normalize：

```python
from deepstrike.runtime.session_repair import repair_events_for_recovery

events = await session_log.read("pay-bug-42")
repaired = repair_events_for_recovery(events, max_bytes=100_000)
```

`repair_events_for_recovery` 会：

- sanitize `llm_completed.content`
- backfill `token_count`
- 保留原始 `provider_replay`
- 不合成 provider-specific replay shape

## Level 7：恢复 workflow 进度

动态 workflow 需要恢复两类信息：

```python
from deepstrike.runtime.session_repair import (
    recover_completed_workflow_nodes,
    recover_submitted_workflow_nodes,
)

events = await session_log.read("wf-session")
completed = recover_completed_workflow_nodes(events)
submissions = recover_submitted_workflow_nodes(events)

outcome = await runner.run_workflow(
    spec,
    session_id="wf-session",
    resumed_completed=completed,
    resumed_submissions=submissions,
)
```

这会跳过已完成节点，并重新应用运行时 append 的节点。

## Level 8：OS Snapshot

`rebuild_os_snapshot_from_session_events` 把 session events 折叠成一个状态摘要：

```python
from deepstrike.runtime.os_snapshot import rebuild_os_snapshot_from_session_events

events = [e.event for e in await session_log.read("pay-bug-42")]
snapshot = rebuild_os_snapshot_from_session_events(events)
print(snapshot.process_by_agent)
print(snapshot.tool_gated_count)
```

它适合 dashboard / debug 页面，不是 kernel snapshot 的替代品。

## 边界

| 能力 | 是否由 SessionLog 保证 |
|------|------------------------|
| append-only 证据链 | 是 |
| 多进程强一致写入 | 取决于你的 `SessionLog` 实现 |
| provider native replay | 取决于 provider 是否实现 descriptor / replay hooks |
| workflow completed node 恢复 | 是，基于 `workflow_node_completed` |
| 动态 append 恢复 | 是，基于 `workflow_nodes_submitted` |
| 文件系统 / tool side effects 回滚 | 否；需要工具自己设计幂等或补偿 |

## 验证入口

- `python/tests/test_session_recovery.py`
- `python/tests/test_provider_replay.py`
- `python/tests/test_replay_fixture.py`
- `python/tests/test_workflow_resume.py`
- `node/tests/provider-replay.test.ts`
