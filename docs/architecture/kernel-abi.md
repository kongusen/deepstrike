---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  rust: [WireEnvelope, KernelInput, KernelEffect, KernelTerminal, KernelTransaction, SyscallRequest]
  python: [RuntimeRunner, KernelJournal, PayloadStore]
---

# Canonical Kernel ABI

Canonical Kernel ABI 是宿主与 [Agent Process Runtime](./agent-process-runtime) 内核之间唯一的稳定边界。Agent OS 描述它的内核分层。宿主拥有 provider、工具、凭据、文件系统、payload 和持久化 I/O；内核拥有 operation lifecycle、effect identity、权限、调度、Context VM 和终止裁决。

## Envelope

每次输入都带 operation、input 和观测时间的关联标识。`WireU64` 使用十进制字符串，union 严格拒绝未知字段和未知 variant。

```json
{
  "operation_id": "op-42",
  "input_id": "input-7",
  "observed_at_ms": "1785542400000",
  "input": {
    "kind": "resolve_effect",
    "effect_id": "op-42:step:1:effect:0",
    "outcome": {
      "status": "failed",
      "failure": {
        "kind": "transport_exhausted",
        "message": "provider unavailable",
        "retryable": true
      }
    }
  }
}
```

内核只接受这一种 envelope，不协商、不降级，也不通过 adapter 恢复旧 operation。

## 五类输入

| 输入 | authority | 用途 |
|---|---|---|
| `ConfigureOperation` | host | 一次性安装 resolved operation config，并形成 genesis record |
| `StartOperation` | host | 以 `RootEntry::Agent` 或 `RootEntry::Workflow` 原子启动 root，并携带 initial context |
| `ResolveEffect` | host executor | 回灌任意 effect 的成功结果或统一 typed failure |
| `DeliverExternalEvent` | external | 递送 signal 或 child completion，causation 由 kernel 校验 |
| `HostControl` | host | cancel、deadline、task update、封闭 policy patch 等 live mutation |

`StartOperation` 是唯一 root 入口。Agent root 的首个 effect 是 `CallProvider`；Workflow root 的首个 effect 是 `SpawnTasks`。session identity 不进入 wire，由宿主 runner 自行映射。

## Effect 与 Terminal

`KernelEffect` 只表达待宿主执行的意图。宿主按 effect ID 幂等执行，并通过 `ResolveEffect` 回灌：

- `CallProvider`
- `ExecuteTools`
- `RequestApproval`
- `SpawnTasks` / `PreemptTasks`
- `PersistMemory` / `QueryMemory`
- `ArchivePageOut` / `LoadPayload`
- `EvaluateMilestone`

Terminal 不是 effect。每个 transition 的 disposition 要么是一组 effects，要么是一个 `KernelTerminal`，两者不能同时出现。

## Durable Transition

宿主不直接 step：

```text
prepare canonical envelope
  -> append core-owned record bytes with CAS
  -> commit using record digest
  -> publish effects or terminal
```

record 保存 normalized input、previous digest、record digest 和 step digest，不保存完整 step。append 成功后 commit 失败时，runner 从 journal rebuild；append 前失败才允许 abort prepared candidate。

## Checkpoint 与 Payload

恢复使用 logical checkpoint + bounded journal tail。checkpoint 按 transition、P1 syscall、P2 scheduler、P3 context VM 分区；candidate、install、ack 和 prefix prune 都使用明确的 CAS/ack 边界。

大结果正文先由宿主写入 `PayloadStore`，内核只接收 opaque locator、digest、size 和 bounded preview。page-in 只能由对应 handle 产生 `LoadPayload` effect；SessionLog 不参与 payload lookup。

## Syscall Causation

模型发出的工具调用通过 pending provider effect 推导 caller。Canonical syscall 包括 invoke、spawn、`AppendWorkflowNodes`、memory proposal 和 page-in；host 不能补写 actor，也不能伪造 root workflow 或 child attempt。

## 延伸阅读

- [执行模型](./execution-model)
- [Session 与恢复](./session-replay)
- [One Canonical Mechanism ADR](../decisions/006-one-canonical-mechanism)
