# 0.2.70 Evolution Runtime

0.2.70 把 Framework Verifiable Runtime 推进为 Evolution Runtime。框架可以证明一个新操作使用了哪组 artifact、候选版本为何被提出、评估证据是什么，以及 promotion decision 为什么允许激活。

```text
ArtifactVersion
  → EvolutionProposal
  → EvaluationRun / EvaluationFact
  → PromotionDecision
  → ArtifactSet activation
  → Verifiable Operation
```

Artifact bytes 仍由 host 的不可变 CAS 持有。内核只接收经过校验的 content-addressed 引用，并在 operation genesis 固定 `artifact_set_digest` 和 promotion decision reference。运行中的 operation 不会热替换 artifact set；激活从新的 operation boundary 开始。

演进对象的 authority 属于 host 的 ArtifactStore、EvaluationStore 和 EvolutionLedger。内核负责 canonical bytes、digest integrity、激活因果和 replay binding。SessionLog、Checkpoint、SDK mirror 和 report 都是 evidence 或 projection，不能取得 proposal、artifact 或 promotion 的第二语义权威。

SDK façade 保持这个边界：`EvolutionRuntime.validate(bundle)` 委托 Rust 的 E1–E8 验证器，`validateStore(store)` 读取 host 持有的 bundle，`activate(bundle, operationId)` 只有在 canonical report 通过后才返回 binding。Node、Python 和 WASM 使用同一套与存储无关的契约；artifact bytes 和 store 实现留在 SDK 之外。

Canonical runner 接受可选的 host `artifactSetDigest`。省略时只记录命名过的 bootstrap identity，这只适用于框架 bootstrap operation；被 promotion 或 replay 的 artifact set 必须显式传入 content-addressed digest。

内核 ABI 是唯一受支持的契约。旧 journal、checkpoint、report 和 evolution format 没有协商、猜测或迁移路径；需要继续运行旧数据的部署必须留在 0.2.69。当前验证器按 E1–E8 拒绝篡改 digest、断裂 lineage、错误 proposal binding、不完整 evidence、无效 regression、未满足 gate 以及越过 boundary 的 activation。

实现依据：[0.2.70 spec](../specs/runtime-evolution-0.2.70.md) · [ADR-010](../decisions/010-evolution-runtime-hard-cut.md) · [Framework Verifiable Runtime](./verifiable-runtime)
