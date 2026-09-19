# ADR-012：可验证 Context 系统

## 状态

已批准并实施。本轮实现验收已通过，结果见关联规格；发布流程另行执行。

## 日期

2026-09-19

## 背景

已有分区 Context、压缩、cache 边界和 measurement 侧表支持运行时优化。可验证执行还需要标识每次 provider attempt 消费的 state、选择、projection 和 host 事实。Kernel 发出 provider effect 时，并不知道 host 最终选择的路由与原生 prompt count。

## 决策

采用两阶段准备，canonical 校验与哈希只由一个 Rust 实现负责。

1. `ContextState` 保持 kernel 语义权威。Renderer 产生精确选择轨迹，driver 将 `ContextCandidate` 冻结到 `CallProviderEffect` 必填字段。Candidate 绑定 state、policy、selection、budget 与 wire `(context, tools)` digest。
2. Host 解析路由、编码请求并取得带 fingerprint 的 preflight count，通过共享 core/JSON 边界调用 Rust `prepare_context_dispatch`，生成 `ContextPlan`、`ContextExecutionInput` 与组件 evidence。在 provider I/O 之前持久化 `context_prepared`。

不新增第二种 kernel effect，不虚构 provider route 或原生 count 占位值。`ContextExecutionInput` 在 kernel effect 发出之后、provider I/O 之前冻结。Provider 原始请求 bytes 与 native usage 仍属于 host evidence。SDK 只镜像契约并委托 Rust 完成输入，不独立实现 selection 或 canonical digest 规则。

`ContextManager::prepare_execution_input` 保留为接收 reference digest 的底层 API。生产执行使用能够校验 host evidence 的 dispatch finalizer。

## 备选方案

- **让 provider render 成为权威**：不采用。传输视图不能成为语义状态的第二写入方。
- **继续隐式选择**：不采用。相同 render 内容无法证明重复源 entry 中哪一个被省略，或另一个为何压缩。
- **在 kernel effect 内冻结完整输入**：不采用。Host 路由与 count 尚不可知，占位身份会声称并不存在的证据。
- **添加第二种 preparation effect**：无需增加。Candidate 可随现有 provider effect 冻结，派发本来就由 host 负责。
- **将原始 provider request 存进 kernel ledger**：不采用。Host 保存 provider evidence，kernel 通过常规 step digest 绑定 candidate。

## 影响

Kernel replay 通过普通 effect/step 校验重建 candidate。Host 可以使用相同 finalizer 重新绑定已记录 route 与 measurement，并将得到的 execution-input identity 与 `context_prepared` 比较。

`EvaluationContextBinding` 绑定 execution input 及诊断组件引用。现有 evaluation validation 检查 binding 完整性与 evidence 覆盖，但 binding 本身不代表已经自动加载 session evidence、回放每个 attempt 或完成评估比较。Evaluation host 必须显式提供 comparison evidence。

0.2.70 hard cut 允许删除旧 Context snapshot 与 SDK constructor。完整 SDK、checkpoint/replay、篡改、持久化顺序和文档检查仍是发布验收要求，不能仅凭类型定义已经存在推断通过。

参考：[Context System 0.2.70](../specs/context-system-0.2.70.md) · [Context Contract](../architecture/evaluation-context.md)

日志保留能力由 host 存储决定：默认内存日志不承诺跨进程持久性，持久化审计需配置 FileSessionLog 或等价存储。
