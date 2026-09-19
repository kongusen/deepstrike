# Evaluation Runtime 中的 Context 契约

Context 通过两阶段边界，同时服务运行时优化和可验证执行。

```text
ContextState → CallProviderEffect 中的 ContextCandidate
  → host route + 带 fingerprint 的 preflight measurement
  → Rust prepare_context_dispatch
  → ContextPlan + ContextExecutionInput
  → 持久化 context_prepared → provider I/O
```

## 权威与准备边界

`ContextState` 是 kernel 的语义权威，索引有序分区内容、task state、signals 与 generation。`ContextPolicy` 提供已解析的运行时控制输入。Renderer 在源索引上记录选择轨迹，包括省略、压缩与分页。

Driver 将这些事实冻结到必填的 `CallProviderEffect.context_candidate`。其 `rendered_snapshot` 覆盖 canonical wire `(context, tools)` tuple，cache 边界对应 wire stable、knowledge 与前缀 turns。正常 effect/step digest 绑定整个 candidate。Kernel 不虚构 provider route 或原生 count，也不引入额外的 kernel effect。

Host 解析实际路由与 request fingerprint，取得 preflight measurement，再调用共享的 Rust `prepare_context_dispatch`。此调用验证 projection 与 evidence 关联，完成 `ContextPlan` 和 `ContextExecutionInput`。Node、Python、WASM 通过 core bridge 调用，不自行实现 selection 或 hashing 规则。

返回的 state、plan、route、measurement、execution input 和 binding 随 `context_prepared` session event 在 provider I/O 之前保存。原始编码请求 bytes 与 postflight usage 仍是独立 host evidence。底层 `ContextManager::prepare_execution_input` 接收 reference digest，不承担生产路径的 host evidence 校验职责。

## Evaluation binding

`EvaluationContextBinding` 将冻结输入投影到评估 evidence，覆盖 `execution_input`、`context_state`、`context_policy`、`context_plan`、`rendered_snapshot`、`prompt_measurement`、`provider_route` 和可选 `cache_prefix`。每项引用必须出现在 `EvaluationRun.evidence_refs` 中，每个被评估 operation 至少有一个 binding；多个 step 可以绑定不同 execution input，同一 input 不可重复。

这些检查验证 binding 完整性与覆盖，不自动加载 session record、回放所有 provider attempt 或比较其输入。Operation-level binding 不等于完整 attempt trace；evaluation host 需要显式提供 replay/comparison evidence。

## Replay 与验收

常规 kernel replay 通过普通 step-digest 校验重建 candidate。结合已记录 host evidence，共享 finalizer 能重新绑定 candidate，计算 `input_digest` 并与持久化事件比较。即使 provider 输出相同，route、count、projection 或 policy 变化也必须产生不同身份或校验失败。

两阶段实现已通过四 SDK、checkpoint/replay、持久化顺序、篡改与文档检查。本轮验证结果见实现规格；发布流程另行执行。

实现规格：[Context System 0.2.70](../specs/context-system-0.2.70.md) · [ADR-012](../decisions/012-verifiable-context-system.md) · [Runtime Language](./runtime-language)

日志保留能力由 host 存储决定：默认内存日志不承诺跨进程持久性，持久化审计需配置 FileSessionLog 或等价存储。
