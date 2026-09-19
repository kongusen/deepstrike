# DeepStrike 0.2.70 — 可验证 Context 系统

## 状态

规格已批准，kernel candidate、Rust dispatch finalizer、四 SDK bridge 和派发前 session evidence 已实施并通过本轮验收。发布流程另行执行。此契约不表示评估系统会自动回放并比较每一次执行输入。

0.2.70 采用 hard cut，不以保留旧 Context snapshot 或 SDK constructor 为目标。

## 目标与执行路径

Context 同时服务有限预算下的运行时优化和可验证执行输入，并保持唯一的语义权威。由于 provider 路由与原生 prompt count 属于 host，准备过程分为两阶段。

```text
ContextState + ContextPolicy
  → CallProviderEffect 中的 ContextCandidate
  → host 确定路由、编码请求、取得 preflight measurement
  → Rust prepare_context_dispatch
  → ContextPlan + ContextExecutionInput + 组件 evidence
  → 持久化 context_prepared
  → provider I/O
```

`ContextCandidate` 是现有 `CallProviderEffect` 的必填字段。整个 candidate 由正常的 effect/step digest 绑定；不新增第二种 kernel effect，也不在 kernel 中虚构 provider route 或 measurement 占位值。host 事实可用后，调用唯一的 Rust 实现完成输入冻结。

## 权威与对象

| 对象 | 权威与职责 | 内容 |
| --- | --- | --- |
| `ContextState` | Kernel 语义权威，可从 durable state 重建 | system/knowledge/history/state 有序 entry identity 与内容 digest、task state、signals、generation |
| `ContextPolicy` | Kernel 控制输入 | 已解析的预算、压力、选择与压缩配置 |
| `ContextCandidate` | Provider effect 中冻结的 kernel 准备事实 | state、operation/step/sequence、policy digest、精确选择轨迹、预算、压力、cache 边界、wire projection digest |
| `ContextPlan` | Rust 使用 kernel 选择与 host 事实完成的显式决策 | candidate 决策、route identity、measurement references、content-addressed plan identity |
| `ContextExecutionInput` | Provider I/O 前冻结的执行身份 | state/policy/plan/projection/measurement/route/cache digest 与因果身份 |
| Provider projection | 临时 ABI 视图 | wire context 与本次暴露的 tool schema |
| 路由、count、请求 bytes、native usage | Host evidence | 实际 provider/profile、request fingerprint、preflight 与 postflight 事实 |

Render 和 preparation 不隐式修改语义分区。entry token 数始终是 measurement。Renderer 按源索引记录 include、collapse、page_out、omit；重复内容不会让已省略 entry 被误判为选中。projected token 数覆盖最终 render，包括 state turn 与合成 anchor。

`ContextState` 是已有 kernel 内容的 content-addressed 索引，不再复制一份内容存储。generation 随 checkpoint 保存，state digest 也能检测语义变动。现存底层可变 partition 接口本身不代表一次 committed kernel mutation；生产路径在 operation 提交边界捕获 candidate。

## Kernel 准备边界

`ContextManager::prepare_candidate(operation_id, step_id, input_sequence, policy_digest)` 返回 kernel 选择事实及 internal render。Driver 检查该 render 与实际 `LoopAction::CallLLM` 一致，然后用 **wire** `(context, tools)` canonical tuple 生成 `rendered_snapshot`，覆盖本次暴露的工具。cache prefix 使用 wire stable、knowledge 与对应前缀 turns 计算。

`runtime_inputs` 另外绑定侧表 measurement、handle residency、knowledge 生命周期与 cache 边界等优化输入。这些输入及 pending knowledge 随 checkpoint 恢复；resident text、durable blocks 和 payload preview 的表示差异不会改变同一内容的语义身份。

Candidate 中的 kernel token estimate 不冒充 provider 原生 count。state、选择轨迹、预算和 policy 都是可重现的 kernel 事实；这些事实变化会改变 candidate 以及 effect/step 身份。

## Host 完成边界

生产路径调用 `prepare_context_dispatch`，并通过共享 Rust JSON bridge 暴露给 Node、Python、WASM。请求携带完整 `CallProviderEffect`、按 `request_fingerprint_scope` 声明范围计算的请求 fingerprint、实际 provider route 对象，以及包含 input count、source、confidence 和相同 request fingerprint 的 preflight measurement。

Core 校验冻结的 wire projection 和 host evidence 关联，再生成 `ContextPlan`、`ContextExecutionInput`、`EvaluationContextBinding`。返回结果同时携带 state、plan、route、measurement 组件内容，便于保存与审计这些引用。`context_prepared` session event 在 **provider I/O 之前**持久化，记录 preparation 与 effect identity。若需要精确 provider bytes，host 另行保留编码后的请求。Preflight、provider usage 和 normalized settlement measurement 是三种独立事实。

独立的 `ContextManager::prepare_execution_input` 保留为接收 reference digest 的底层 core API。它不负责路由解析、请求编码，也不单独验证 host evidence 内容。生产派发使用上述共享 finalizer。

默认 session log 只提供内存生命周期内的保留；需要跨进程恢复与审计时，必须配置 FileSessionLog 或等价的 host 持久化存储。日志写入失败会阻止 provider 调用。内置 provider 冻结编码后的请求，并由 count 与 stream 复用；Rust 使用 `prepare_context_request` 冻结请求体，并由 `stream_prepared` 发送同一份内容；指纹同时绑定治理过滤后的输入和 provider continuation state。自定义 provider 的默认 `adapter_input` scope 只证明逻辑适配器输入，Node/WASM 对应 `prepareRequest`，Python 对应 `prepare_request`。默认适配器的 state 必须是可记录的 JSON 数据；存在隐藏编码状态或非 JSON 状态时，provider 必须实现自己的准备边界。`rendered_snapshot` 绑定 kernel projection，host 变换后的请求由 request fingerprint 绑定，二者不可混同。

## Replay 与 Evaluation

常规 kernel replay 重建 `ContextCandidate`，通过正常 effect/step digest 验证。把重建的 candidate 与已记录的 host route、measurement 交给相同 Rust finalizer，可以重算 `ContextExecutionInput.input_digest`，并与 `context_prepared` 比较。host 执行该比较时，即使 provider 输出相同，输入 digest 不同仍应报告不一致。

`EvaluationContextBinding` 将 execution input 投影到评估 evidence，覆盖 `execution_input`、`context_state`、`context_policy`、`context_plan`、`rendered_snapshot`、`prompt_measurement`、`provider_route` 和可选 `cache_prefix`。现有 validator 检查 binding 完整性、每个被评估 operation 至少一个 binding、同一 operation 不可重复相同 execution input，以及 evidence reference 覆盖。

Validator 不会自动加载所有 session event、回放 operation 或比较全部 provider attempt 输入。Evaluation host 需要显式提供 replay/comparison evidence。一个 operation-level binding 也不等同于该 operation 的完整 provider attempt 轨迹。

## 发布验收

连接完成的实现仍需验证以下行为：

- state、candidate selection、policy、wire context/tools 的改变影响记录身份。
- 未知或重复 entry reference、损坏的 plan、错误 generation 被拒绝。
- 实际 projection 与冻结 candidate 不一致时拒绝派发。
- measurement 的 request fingerprint 不一致时拒绝派发。
- cache claim 确实对应冻结 wire projection 的前缀。
- `context_prepared` 持久化先于 provider execution。
- checkpoint restore 与确定性 replay 重建相同 candidate。
- 已记录 host 事实生成相同 execution input；篡改事实产生新身份或校验错误。
- SDK 委托同一个 Rust finalizer，不自行实现 digest 或 selection 规则。
- evaluation coverage 与组件 evidence reference 验证保持 fail-closed。

Provider 原始 bytes 始终属于 host evidence。此契约不会把 kernel 变成 provider router，不引入另一个 scheduler，也不将 host 提供的观测视为已获独立真实性证明。

## 本轮验证（2026-09-19）

- Rust workspace：1471 passed，25 ignored（其中 core 1137、Rust SDK 128）。
- Node：1054 passed，14 skipped；Python：739 passed，2 skipped；WASM：193 passed。
- Cross-SDK conformance：24 fixtures × 4 SDK，14 项由 runner 的适用范围规则跳过，其余全部通过。包含 Context execution 与 encoded request/continuation 指纹。
- checkpoint/replay、恢复差分、日志失败阻断、编码冻结、measurement 隔离及非 JSON state 拒绝均有回归覆盖。
- docs drift：157/157 paths、274/274 symbols、63/63 中英对应；docs build、format 与 diff check 通过。
