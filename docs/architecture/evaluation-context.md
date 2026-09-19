# Evaluation Runtime 中的 Context 契约

Context 在 Evaluation Runtime 中不是只负责压缩和 token 压力管理的临时字符串。它是一次
评估输入的可验证投影，必须能回答同一个 operation 使用了什么策略、接收了什么输入、渲染了
什么 prompt，以及 provider 侧采用了什么测量。

## 权威边界

- Kernel 拥有 `ContextPartition`、解析后的 `ContextPolicy`、输入接纳顺序和 durable state。
- Renderer 是 Projection，只能从 canonical state 生成 provider-facing context；它不能成为第二个
  Context authority。
- `ContextTokenEngine` 和 host provider usage 是 Measurement。测量结果经过 normalize 和
  Settlement 后才影响预算或 accounting。
- 原始 context bytes、provider request 和 usage 保留在 host evidence plane，不进入 Evolution
  ledger 的大对象字段。

## EvaluationContextBinding

每个被评估的 operation 都必须有一个 `EvaluationContextBinding`，并由 `EvaluationRun.contexts`
携带。它绑定以下 digest：

| 字段 | 语义 |
| --- | --- |
| `context_policy` | 解析后的 context policy 身份 |
| `input_snapshot` | 被评估的 canonical 输入快照 |
| `rendered_snapshot` | renderer 产生的 provider-facing 投影 |
| `prompt_measurement` | prompt token 或 provider usage measurement |
| `cache_prefix` | 可选的 cache prefix 身份；存在时也必须有 evidence |

Binding 的 digest 还包含 `operation_id`。核心 E1–E8 validator 会拒绝 digest 被篡改的 binding、
没有对应 operation 的 binding、没有覆盖全部 operation 的评估，以及没有在
`EvaluationRun.evidence_refs` 中登记上述证据的评估。

因此评估比较必须固定同一 dataset、artifact set 和 Context binding。改变 policy、召回内容、
渲染结果或 measurement 供应方都会产生新的 binding，不能只更新一个 token 数字继续复用旧的
EvaluationRun。

## 与 replay 的关系

Replay 读取 durable input 和 host evidence，重新生成 renderer projection，并将 digest 与
`EvaluationContextBinding` 比较。SDK 只提供 mirror 和 host store adapter；Node、Python、WASM
都委托 Rust core 的 canonical validator。Context bytes 的存储位置可以变化，但 binding 的字段、
digest 计算和证据要求不能由 SDK 各自解释。

实现参考：[Evolution Runtime](./evolution-runtime)、[Runtime Language](./runtime-language)、
[ADR-011](../decisions/011-evaluation-context-contract.md)。
