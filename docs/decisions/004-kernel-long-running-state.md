# ADR-004：长生命周期内核状态与外置结果协议

## 状态

Accepted

## 日期

2026-07-15

## 背景

ABI v2 已把 operation、event、effect、signal、budget、cancellation 和 portable replay 收敛到内核，但完整 input journal 仍使恢复成本随运行总长度线性增长；大工具结果也会先完整穿过 ABI、进入 journal，再由宿主 spool。条数限制只能保护内存，不能让长任务持续生成可恢复检查点。

## 决策

### 1. 事务执行使用单一路径

内核事件处理收敛为 `normalize -> validate -> plan -> commit -> journal`。runtime lifecycle、pending effect、step sequence、actions 与 observations 必须由同一 transition plan 提交。fault 不提交任何 runtime-owned 状态。状态机不提供第二套 lifecycle 裁决接口。

### 2. 大结果直接使用 inline 或 external payload

`ToolResults` 最终只接受封闭联合：

- `inline`：小结果正文；
- `external`：`blob_ref`、digest、original size 与 preview。

SDK 在提交 external input 前原子持久化正文，内核验证 payload 与 configured threshold。旧 `SpoolLargeResult` action/result、pending kind 和 retry 分支直接删除，不保留兼容路径。文件、对象存储和加密仍由宿主拥有。

### 3. 快照使用逻辑 checkpoint 与 bounded tail

新的 checkpoint schema 独立于 `LoopStateMachine` 私有布局，包含版本化逻辑状态 DTO、base step、bounded tail inputs、pending effects、replay metadata、resource policy 与 state/tail digest。恢复先装载逻辑状态，再只回放 tail；成功 checkpoint 后 journal 可 rebase，恢复复杂度取决于 tail 而非运行总事件数。

旧的 full-journal snapshot 直接由新格式替换。宿主只持久化 opaque checkpoint，不读取或修改内部逻辑状态字段。真实性与加密由宿主负责；内核 digest 用于损坏和不一致检测。

### 4. 资源边界同时按条数和字节执行

SDK 只配置有宿主资源语义的限制。当前已实现 `max_input_bytes`、`snapshot_input_limit`、`snapshot_journal_bytes_limit` 与只读 `KernelDiagnostics`。后续 checkpoint tail 延续同一字节水位，不开放容器容量等实现参数。

## 实施顺序

1. 恢复热路径与字节资源诊断；
2. transition plan 与 lifecycle 单一事实源；
3. external tool-result payload，删除 spool legacy；
4. logical checkpoint + bounded tail + digest；
5. 增量 render cache，仅在端到端 benchmark 证明收益后实施。

每步独立测试、验证和提交。不会在同一个提交中同时重写 wire payload 与 snapshot schema。

## 影响

- 长任务不再因为固定 journal 条数永久失去快照能力；
- 大结果正文不进入内核 journal 或 portable checkpoint；
- fault/panic 边界不留下 runtime-owned 半提交元数据；
- SDK 只保留一套结果和快照协议；
- checkpoint schema 需要独立 golden、跨 Node/Python/Rust/WASM parity 和 uninterrupted/restore differential 测试。

## 非目标

- core 不执行文件、网络、数据库或对象存储 I/O；
- core 不保存 API key、lease token、加密密钥或可执行 handle；
- 不为旧 spool 或 full-journal snapshot 提供 adapter；
- 不以无界缓存换取恢复或 render 性能。
