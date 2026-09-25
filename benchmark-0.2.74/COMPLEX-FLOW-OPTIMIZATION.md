# 复杂业务流程的体系化优化方案

这份方案基于 Node SDK `0.2.74` 的当前契约、确定性 benchmark、真实 provider 测评和 `dynamic-complex` 结果。目标是让动态 workflow 能承载可恢复、可审计、可控副作用的业务流程，而不把业务正确性寄托在模型最终生成的一段文字上。

## 当前结论

当前机制已经能表达复杂流程的主要控制结构：phase、串行 pipeline、受限 fan-out、条件分支、聚合、审批、pause/resume 和 replay。动态 workflow 由 host 脚本控制，调用 `agent()` 或 `parallelAgents()` 时向 kernel 提交 workflow batch；静态 workflow 则拥有 `dependsOn`、`depPolicy`、`reducer`、`loop`、`classify`、`tournament` 和节点级预算等更强的 DAG 语义。

因此，现阶段适合采用混合架构：动态层负责规划和调度，静态 DAG 负责强依赖、分类、竞赛、聚合和可审计执行，业务状态机和副作用日志负责最终一致性。

## 优先级总表

| 优先级 | 问题 | 影响 | 优化方向 | 验收信号 |
| --- | --- | --- | --- | --- |
| P0 | 动态脚本的时序不等于 kernel 的依赖边 | 中断恢复时可能无法准确判断哪些节点可继续 | 动态 plan 显式携带依赖、阶段和节点状态，kernel 持久化依赖图 | 重启后只恢复可执行节点，禁止重复执行已完成依赖 |
| P0 | batch 生命周期事件不成对 | trace 无法可靠计算活跃数、延迟和失败率 | 每个节点发出配对的 `agent_started` / `agent_completed`，附带 batch、sequence、attempt | 所有终态节点都有且只有一个开始事件和一个终态事件 |
| P0 | replay 只复用模型结果，不能保护外部副作用 | 重放可能重复发消息、写库、扣款或创建工单 | 为副作用引入 effect journal、幂等键和 replay policy | 相同 run replay 的副作用提交数为 0，且审计记录可关联到原 effect |
| P0 | `outputSchema` 只约束形状 | JSON 合法不代表业务正确 | 增加业务 verifier、状态不变量和人工确认门 | 故意注入错误字段时流程被 verifier 拒绝，并保留原因 |
| P1 | 动态 API 没有直接暴露完整 DAG 语义 | 复杂流程被迫拆成 host 时序，难以静态检查 | 扩展 dynamic node options：`dependsOn`、`depPolicy`、`reducer`、`classify`、`tournament` | 动态提交能生成与静态 workflow 等价的依赖和聚合 trace |
| P1 | approval 目前主要在 workflow 入口 | 高风险节点或工具不能在中途单独拦截 | 分层审批：workflow、node、tool、effect | 未批准的危险节点不进入 provider，审批事件能定位到节点和工具 |
| P1 | pause 是协作式边界暂停 | provider 或工具正在执行时不能立即停下 | 定义可观察的 checkpoint、取消传播和 in-flight 状态 | pause 后不再提交新工作；取消信号最终传到 provider/tool |
| P1 | replay store 偏向进程内或测试实现 | 进程崩溃后无法稳定恢复 | 生产默认使用持久化 replay store，并保存 artifact、输入、限制和版本指纹 | 强制崩溃后恢复不丢节点、不重复副作用 |
| P1 | 并发上限和配额的作用域需要统一 | 嵌套动态调用可能绕过预期的全局预算 | 建立 run 级 admission controller，统一节点数、并发、token、wall time | 嵌套 fan-out 仍受同一全局上限约束，并给出可解释拒绝原因 |
| P2 | 规划、执行、验证都使用同一模型成本高 | 延迟和 token 成本不可控 | planner/worker/verifier 分工，使用 `modelHint`、token budget 和路由策略 | 同等正确率下 token、P95 延迟和 provider 错误率下降 |
| P2 | 缺少流程级静态检查 | 周期性错误只能到运行时才暴露 | 增加 workflow lint：循环依赖、缺失依赖、无界 fan-out、无幂等副作用、预算不可行 | 发布前能阻断结构性错误，并输出节点定位 |

## P0：先保证正确性和可审计性

### 1. 把业务结果和模型结果分开

每个关键节点应同时产出三类信息：模型结果、结构化事实、验证结果。`outputSchema` 负责结构；verifier 负责业务不变量；状态机负责是否允许进入下一阶段。verifier 应能检查金额、权限、库存、时间窗、引用对象是否存在，以及聚合结果是否覆盖全部必需输入。

建议增加统一的节点契约：

```ts
type VerificationContract = {
  outputSchema: Record<string, unknown>
  requiredFacts: string[]
  invariants: string[]
  verifier: string
  onFailure: "retry" | "branch" | "manual_review" | "fail"
}
```

benchmark 需要覆盖合法形状但违反业务规则的结果，确保流程进入明确的失败、重试或人工审核分支。

### 2. 建立 effect journal

所有写操作、通知、支付、创建外部资源等副作用都必须先登记 effect，再执行 effect。幂等键至少由 `runId`、`nodeId`、业务对象标识和操作类型组成。replay 时根据策略选择 `reuse`、`inspect` 或 `reexecute`，默认不重放不可安全确认的副作用。

建议把以下字段写入 kernel 审计事实：`effectId`、`idempotencyKey`、`target`、`requestDigest`、`status`、`providerReceipt`、`attempt`。这样模型重试、workflow replay 和 provider 超时都不会导致无界重复操作。

### 3. 修正生命周期事件

当前复杂动态流程的执行计数是正确的，但 `parallelAgents()` 的 batch trace 已观察到存在 `agent_completed` 而没有对应 `agent_started` 的情况。这会直接破坏 active、延迟和成功率指标。修复后应为每个 admitted node 发出唯一的开始事件，终态统一发出 completed、failed、skipped 或 cancelled 之一，并加入 `batchId`、`sequence`、`attempt` 和 `replay` 字段。

## P1：统一动态调度和静态 DAG

动态层适合决定“下一步要做什么”，kernel 需要知道“哪些条件满足后才能做”。建议把 dynamic plan 从当前的节点列表扩展为带依赖和语义的计划：

```ts
type DynamicNodePlan = {
  nodeId: string
  dependsOn: string[]
  depPolicy?: "all" | "any" | "best_effort"
  reducer?: "concat" | "dedupe_lines" | "merge_json_arrays" | "count"
  phase?: string
  sideEffect?: { idempotencyKey: string; replay: "reuse" | "inspect" | "reexecute" }
}
```

动态脚本仍然可以分阶段生成计划，但每次提交都要让 kernel 持久化新增边、计划版本和 admission 结果。对于有强依赖、分类、竞赛或 reducer 的流程，优先下沉到静态 DAG；动态层只负责生成下一批节点和选择分支。

`ctx.parallel()` 目前是 host 侧计算并发，不等同于 kernel 记录的 Agent fan-out。API 和 trace 中应明确区分 host operation、agent node 和 external effect，避免把本地并发误报成 provider 并发。

## P1：恢复、暂停和治理

恢复路径应按 checkpoint 工作：提交 batch、收到 admission、每个节点终态、审批决策、effect receipt 都写入持久化状态。进程重启后先读取未决 batch，再根据节点状态和 effect journal 决定等待、复用、补偿或失败，不能简单从脚本开头重跑。

审批需要从 workflow 入口扩展到 node、tool 和 effect 四层。危险能力应在 admission 前被阻断；工具调用和外部写操作要能携带租户、请求、操作者和策略版本。配额也应是整个 run 的全局预算，而不是单个 `parallelAgents()` 调用的局部计数。

pause 的契约需要明确为：已在执行的 provider/tool 是否允许完成、何时停止提交新工作、取消信号如何传播、恢复后哪些节点可重试。benchmark 应加入长调用、工具阻塞、取消和恢复场景。

## P2：质量、成本和运维

规划模型负责拆解和分支，worker 模型负责窄任务，verifier 使用更稳定或规则化的检查器。给节点配置 `modelHint`、token budget、max turns 和 max wall time，并为知识、memory、skill 设定按需加载预算。稳定的系统指令和 skill metadata 可以缓存，聚合尽量在 host/kernel 完成，减少把中间结果重新交给模型。

增加 provider 健康度、超时、重试和熔断指标，路由时记录 provider、模型、请求指纹和降级原因。流程发布前运行 lint，阻断循环依赖、无界 fan-out、无幂等副作用节点、缺失 verifier 和预算不可行的计划。

## 推荐落地顺序

1. 修复动态生命周期配对，并把 `batchId`、`sequence`、`attempt` 纳入 trace。
2. 为 dynamic plan 增加显式依赖、phase 和 side-effect 元数据，保存到 kernel replay fact。
3. 实现 effect journal 和幂等执行器，先覆盖通知、写库、创建资源三类副作用。
4. 引入 verifier contract benchmark，覆盖 schema 合法但业务错误、部分输入缺失和人工审核分支。
5. 把 replay store、checkpoint、pause/cancel 和崩溃恢复做成持久化场景。
6. 统一全局 admission controller，再测嵌套 fan-out、预算和取消传播。
7. 最后做模型路由、缓存、provider 熔断和流程 lint，避免在正确性未稳定前过早优化成本。

## Benchmark 验收矩阵

每次 SDK 变更至少运行以下矩阵：

| 场景 | 必须证明的事实 |
| --- | --- |
| `dynamic-complex` | 5 个阶段、受限 fan-out、条件分支、聚合、审批、pause/resume 全部可重放 |
| lifecycle balance | 每个 admitted node 的开始和终态事件严格配对 |
| verifier rejection | 业务不变量失败时不会进入下一阶段，失败原因可定位 |
| side-effect replay | 相同 run replay 不重复执行 effect，effect receipt 可复用 |
| crash recovery | 在 batch、审批、effect 三个 checkpoint 崩溃后可恢复 |
| quota and cancellation | 嵌套 fan-out 受全局预算约束，取消最终传递到 provider/tool |
| provider matrix | OpenAI、MiniMax、Kimi 的能力和失败模式分别记录，不把模型未触发工具误判为 SDK 失败 |
| progressive skill | metadata、skill body、references/assets/scripts/examples 按需加载，未授权资源不可见 |

建议把这些指标写入 JSON artifact，并为 `dispatchCount`、`reusedCount`、`duplicateEffectCount`、`unpairedLifecycleCount`、`verificationRejectCount`、P95 latency、token usage 和 completion warnings 建立 baseline。这样后续优化可以区分功能正确性、可靠性、成本和模型行为差异。

