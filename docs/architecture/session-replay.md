# Session 与重放

Agent OS 的 **可重放性** 来自：控制流状态在内核可序列化 + 宿主把每一步写成 SessionLog event。这不是「保存 chat history」那么简单。

## 为什么 Session 是 OS 级能力

脚本 harness 的编排状态往往在：

- 闭包变量
- 临时 JSON 文件
- orchestrator 进程的内存

DeepStrike 把 **可恢复边界** 定义为：

```text
SessionLog (append-only evidence)
    +
KernelSnapshot / event replay
    +
宿主侧 store (DreamStore, ArchiveStore, FileSessionLog)
```

内核 **不** 持久化到磁盘——SDK 拥有 I/O——但内核 **产出** 可持久化的 observation。

## SessionLog

| 实现 | 用途 |
|------|------|
| `InMemorySessionLog` | 开发 / 单测 |
| `FileSessionLog` | 生产持久化 |

每条 entry 对应一次或一批 `KernelObservation`，外加宿主事件（如 `llm_completed`）。

典型 event kind：

| kind | 含义 |
|------|------|
| `run_started` / `run_terminal` | run 边界 |
| `tool_invoked` / `tool_denied` | syscall 审计 |
| `agent_process_changed` | TCB / sub-agent 生命周期 |
| `workflow_node_completed` | DAG 推进 |
| `memory_written` | Dream 提交前校验记录 |
| `pressure_compact` | Context VM 压缩 |

## Wake / Resume

挂起态（`TaskState::Suspended`）常见原因：

- Governance `Gate(AskUser)`
- 等待 sub-agent join
- Workflow barrier 未齐

恢复路径：

```python
# 同一 session_id 继续 — SDK 从 log 重建 kernel
async for event in runner.run(goal, session_id=existing_id):
    ...
```

测试参考：`python/tests/test_runtime_wake.py`

**Workflow 特有能力**：运行时 `SubmitNodes` append 的节点也写入 log，resume 后 DAG 包含动态扩展部分。

## Replay 与确定性测试

| 机制 | 用途 |
|------|------|
| `ReplayProvider` | 固定 LLM 输出，跑内核/integration 测试 |
| `rebuild_os_snapshot_from_events` | 从 log 重建 OS 级计数器/快照 |
| `ProviderReplay` | 录制真实 provider 响应后重放 |

Replay 构建 LLM message 时会 **剥离 audit event**，避免污染 provider 视图。

## 与 Context 压缩的交叉

压缩产生 `archived` messages 时：

- 可选写入 `ArchiveStore`（`compression_store`）
- `frozen_prefix_len` 更新 → 影响下一轮 prompt cache

SessionLog 记录 `pressure_compact` 与 `prefix_invalidated_at`，replay 时压缩决策可重建。

## RunGroup 与多 peer

`RunGroup` 把 **多个 session** 绑在同一治理域（累计 spawn / token）：

- 每个 persona 独立 SessionLog
- 共享 `GroupLedger`
- ReactiveSession 从 membership 恢复 peer 集

见 [RunGroup 预算](../concepts/run-group-budget)。

## 延伸阅读

- [执行模型](./execution-model)
- [Kernel ABI](./kernel-abi)
- [Context 工程](../guides/context-engineering)
