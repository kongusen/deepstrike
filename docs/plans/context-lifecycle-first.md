# 上下文优先：DeepStrike 架构转向实施计划

> 修订 2026-11-01：确认允许破坏式更新。被取代的机制直接删除，不保留插件、兼容层或同名模板；迁移说明按版本写进 `MIGRATION-0.2.77-to-0.2.78.md` 等文件。
>
> 修订 2026-11-02：不跳到 0.3.0，本计划分三个版本 0.2.78–0.2.80 发布，见“版本规划”。原定的 0.2.81 并入 0.2.80，作为一次较大的版本更新。

## 概览

DeepStrike 的核心原则从“可验证的 effect 内核”转为：

> 在有限的注意力预算下，为每一条信息决定它的**作用域、形态、寿命和写入权**，并让这些决定能被**结果修正**；副作用另由 **effect 准入**约束。

现状问题（已对照代码核实）：

| # | 问题 | 证据 |
|---|------|------|
| Q1 | 生命周期没有一等对象：recall、skill、依赖输出、工具结果各走各的路径 | `context/partitions.rs`、四个 runner |
| Q2 | 生命周期策略写在宿主侧：`prefetchMemory` 在四个 runner（合计 14.6k 行）里各写一遍，recall 放置、去重、key 规则都在宿主 | `node/src/runtime/runner.ts:4064` 及 Python/Rust/WASM 对应函数 |
| Q3 | knowledge 渲染在 turns 之前，运行中追加会让整段 history 的缓存失效，注释却称之为“cheap direction” | `context/partitions.rs:117` |
| Q4 | 引用检测和淘汰只靠词法重叠 | `partitions.rs:observe_references`、`context/utility.rs` |
| Q5 | 开启 `delegate_compaction` 后，模型可见的上下文和 kernel 计算 pressure 用的不是同一份 | `context/manager.rs:415`、b0dadad0 |
| Q6 | DAG 边是字符串拼接、按字符截断；`summary` 模式实为头尾截取 | `workflow-control-flow.ts:28-57`（已在 2.2 删除） |
| Q7 | Plan/Implement 子 agent 默认 `Full` 继承 | `types/agent.rs:172` |
| Q8 | 动态编排有**三条**路径：声明式 `NodeKind::Loop/Classify/Tournament`、模型运行时 `submit_workflow_nodes`、宿主程序 `dynamic.ts`（仅 Node） | `orchestration/workflow/mod.rs:76`、`node/src/workflow/dynamic.ts` |
| Q9 | 记忆召回只有 turn 1 和 renewal 两个时机，无质量反馈 | 四个 runner 的 `prefetchMemory` 调用点 |
| Q10 | 没有反馈：无从得知哪些淘汰错了、哪些注入白费了 | — |
| Q11 | 每次 provider 调用前都 `await` 写一条 `context_prepared`，日志写入挡在调用之前 | `node/src/runtime/runner.ts:3205` |

## 对上一版的两处修正

1. **不需要放弃确定性重放。** kernel 的恢复是“checkpoint + 重放尾部已记录的输入 + 比对 state digest”（`runtime/kernel/wire/restore.rs:26`）。只要非确定的信号（模型相关性判断、实测 usage、模型摘要、provider compaction）都作为**已记录的输入**进入 kernel，重放天然成立；`UsageFacts` 已经是这样进来的（`wire/event.rs:199`）。所以真正要删的是叠在上面的**上下文证据层**（`ContextExecutionInput` digest、`context_prepared`、shared evidence、verifiable 报告、evolution 绑定），恢复机制本身保留。上一版的任务 0.2（“改为读取已记录决策”）因此取消。
2. **动态编排是三条路径，不是两条**，并且 Loop 已经改用 `pace` meta-tool 返回控制信号。删掉 Classify/Tournament 之后，“从文本解析 JSON 控制信号”的问题随之消失，不需要单独的结构化任务。

## 架构决策

- **D1 kernel 是“已记录输入”上的确定状态机。** 任何非确定信号都先作为 effect 结果或 fact 进入 journal，再由 kernel 决策。kernel 内不允许自己调模型或读时钟。
- **D2 删除上下文证据层。** 不改成插件，直接删除（清单见下）。崩溃恢复只依赖 journal + checkpoint。
- **D3 只有一个上下文对象：`ContextItem`。** 四个分区不再存储数据，而是按 item 属性派生的渲染位置。
- **D4 策略在 kernel，I/O 在宿主。** “放哪、留多久、什么形态”只在 kernel 写一次；宿主只负责取数据（memory store、payload、provider）。
- **D5 每个上下文事实只有一个权威。** provider 改写了模型可见的上下文时，kernel 把改写作为 fact 吸收，不再维护一份虚构的完整 history。
- **D6 动态编排 = 宿主程序或模型提交 + kernel 准入。** kernel 只保留 `Spawn` 和 `Reduce` 两种节点，以及 edge、配额、取消。
- **D7 effect 准入不变。** governance、审批、sandbox、配额是另一条线，本计划不碰。

## 核心模型：ContextItem

```text
ContextItem {
  id / key       稳定身份；带 key 时按 key upsert
  origin         system | skill | memory_recall | tool_result | dependency_output
                 | user | assistant | task_state | signal | provider_summary
  scope          operation | task | edge(from→to) | session | agent
  fidelity       full | structured | handle(locator, preview)
  lease          permanent | until_boundary | turns(n) | until(event) | single_use
  authority      host | kernel | model | provider
  value          { injected_at, last_referenced, ref_count, paged_in_after_evict, judged_relevance? }
  cost           { tokens(measured|estimated), cache_position }
}
```

`volatility` 不再单独存，由 origin + lease 派生；`summary` 不再是独立的 fidelity（见任务 3.1）。

**渲染位置规则：**

| 派生 volatility | 渲染位置 | 允许变更的时机 |
|-----------------|----------|----------------|
| stable | system[0] | 只在 operation 开始时 |
| durable | system[1]（knowledge） | 只在 boundary（renewal/compaction）或 history 为空时 |
| episodic | history | 随时追加到尾部；改写走压缩管线 |
| volatile | state_turn | 每轮重建 |

**缓存不变式（修 Q3）：** history 非空时到达的 durable item，先作为 episodic item 追加到 history 尾部，到下一个 boundary 再提升进 system[1]。唯一例外是调用方显式声明 `urgent`，此时 kernel 记一条 `cache_invalidation{tokens_at_risk}`。skill 激活、renewal recall、宿主钉住的资料都走这条规则。

## 任务

### 阶段 0：删除上下文证据层（修 Q11，解锁后续阶段）

- [x] 任务 0.1：删除运行路径上的证据层（2026-11-01，分支 `feat/context-lifecycle-stage0`）
  - 已删除：`context/execution.rs`、`evolution.rs`；`CallProviderEffect.context_candidate`、`OperationConfig.artifact_set_binding`、checkpoint 的 `state_generation`；`context_prepared` 事件；三个绑定的 `contextPrepareJson`/`contextVerifyJson`/`evolutionValidateJson`；四个 SDK 的 shared evidence、evolution、context preparation 模块与 `artifactSetDigest` 选项；conformance 的 `context_execution`、`shared_evidence_snapshot` 两个域。
  - 附带收益：kernel 每次 provider 调用不再为了 candidate 把上下文渲染第二遍。
  - 已验证：Rust workspace、Node（1422）、Python（912）、WASM（jest 296 + 真实 WASM 内核回归）、四端 conformance（23 fixtures）全部通过。restore 的 state digest 自检保持开启。
- [x] 任务 0.2：删除离线证据工具（2026-11-01）：restore 本身已做链校验、确定性重放和 state digest 自检，chain_validator、verifiable.rs、`deepstrike` CLI、`verifiableOperationJson` 与 SDK verifiable-report 全部删除。
- ~~原任务 0.2：裁剪离线证据工具~~
  - `runtime/verifiable.rs`、`runtime/chain_validator/`、`deepstrike` CLI 以及 SDK 的 `verifiableOperationJson`/verifiable-report 不在运行路径上，0.1 暂时保留。待定：chain_validator 缩为 restore 的回归测试（只留 C1 链完整性、C3 重放确定性），删除 SessionLog 输入平面和 CLI 的 `verify` 子命令。
  - 验证：在压缩后、recall 后、renewal 后三个崩溃点恢复，恢复出的上下文与崩溃前一致。

### 阶段 1：ContextItem 统一模型（修 Q1、Q3）

- [x] 任务 1.1a（2026-11-02）：新增 `context/item.rs`：`ContextItem`（origin / lease / value）与 origin 默认值表（`ItemOrigin::defaults`，11 种 origin 各有 scope / fidelity / lease / authority）。knowledge 分区的条目改为 `ContextItem`，`KnowledgeEntry` 删除；宿主写入的 knowledge 按 key 定 origin（`skill:` → skill，其余 → reference），kernel recall 为 `memory_recall`，治理提示为 `system`。lease 四种：permanent、until_boundary、turns(n)、single_use，到期后标记为在下一个 boundary 离开（不在缓存代内改写 system[1]）。recall 的 index line 默认 `turns(8)`，从最近一次 recall 或引用起算，`memory(record_id)` 读取算作引用并标记 `expanded`。`lease_remaining_steps` 改为从 lease 计算。checkpoint 的 knowledge slot 带上 origin、lease、离开原因和 value，`retired_items` 记录已离开的 item；缺 origin 的 slot 按 key 推断（`memory:` → recall）。未做：`until(event)` lease；scope / fidelity / authority 目前只是表中的描述，尚无行为。
- [x] 任务 1.1b（2026-11-02，部分）：每条 history 消息带 `ItemTag { id, origin, injected_turn }`，id 按到达顺序分配、不复用，随压缩归档、renewal 携带和 checkpoint（`history_items`、`last_item_id`）保留；缺 `history_items` 的 checkpoint 恢复为 id 0。origin 由角色决定（user / assistant / tool_result / system）。handle 仍按 call_id 锚定到 tool_result item，其 residency 即该 item 的 fidelity（Resident = full，Collapsed / External / PagedOut = handle）。`read_result` 读取旧 tool result 或 `dep:` 依赖输出记为 read-back。lifecycle report 的 `history` 按 origin 汇总：进入过 history 的 item 数和 token、结束时仍在 history 的、read-back 次数。
  - 0.2.79 第一项（2026-11-02）：system[0] 与 history 的 `Partition` 改为 item 表，每行是一个 item（`ItemTag`、渲染的消息、宿主测量），按列存储，字段私有，只能按行写入，消息列只读地交给渲染器和压缩规划器。压缩按 item 操作：snip / micro 原地改写的 item 保留 id，fidelity 记为新增的 `excerpt`（checkpoint 的 `history_items` 只在非 full 时写出该字段）；collapse / auto 整行取出，归档结果带 item 标签。顺带修复：rollback 原先只截断消息列，测量、标签与 token 计数会错位，现在整行截断。
  - 0.2.79 其余项（2026-11-02，1.1 完成）：
    - `until(event)` lease：新增 `LeaseEvent`，skill body 的 lease 为 `until(skill_deactivated)`，skill 停用时离开（下一个 boundary 生效）。钉住只延长有时限的 lease（turns、boundary、single_use），不延长事件 lease。
    - authority：宿主的 `seed_knowledge` / `apply_knowledge_mutation` 不能覆盖或删除 kernel、provider 写入的 item，也不能删除仍在激活的 skill body；违反时整条命令以 `InvalidAuthority` 拒绝。
    - scope：`ItemScope::inherited_by` 决定子 run 继承哪些父 knowledge（`full` 继承 operation / task / session / agent，`system_only` 只继承 agent，edge 一律不继承）；事件 lease 的 item 和 kernel 写入的 item 不继承。kernel 在 spawn 的 metadata 里下发 `inherited_knowledge`，Node / Python / WASM 传入子 run。renewal 保留除 edge 外的所有 scope（`survives_renewal`）。
    - handle 并入 item 表：handle 带所属 history item 的 id（checkpoint `HandleState.item`，旧 checkpoint 按 call_id 回填），按 item id 清理。fidelity 决定渲染：body 被折叠或换出的 item 渲染为 handle 预览（`ContextManager::history_fidelity`）。
    - history 逐条明细：lifecycle report 的 `history_items` 列出最近 128 个 history item（id、origin、token、渲染时的 fidelity、离开原因 `compacted` / `renewed` / `removed`），更早的计入 `history_items_omitted`；离开的记录进入 checkpoint（`retired_history`）。
    - 迁移说明：`MIGRATION-0.2.78-to-0.2.79.md`。
- ~~原任务 1.1：kernel 引入 `ContextItem`、item 表和 origin 默认值表~~
  - 验收：`ContextPartitions` 变成 item 表上的渲染视图；`KnowledgeEntry`、history message、handle 都映射为 item，旧类型删除。一张表定义每种 origin 的默认 scope/fidelity/lease/authority（例如 `memory_recall → session / handle(index line) / turns(N) / host`），宿主没有直接写分区的 API。`mm/value.rs` 里一直是 `None` 的 `lease_remaining_steps` 改为从 lease 计算。
  - 验证：现有 renderer 和 compression 测试迁到新模型后通过；每种 lease 有到期测试。
  - 文件：新增 `context/item.rs`；改 `context/partitions.rs`、`manager.rs`、`renderer.rs`、`mm/handle.rs`、`mm/value.rs`

- [x] 任务 1.2（2026-11-01）：prefix 发出后到达的 knowledge 先渲染进 state_turn（staged），boundary 时提升；staged token ≥ history/4 时立即提升并计入 `cache_invalidated_tokens`。`prefix_sent`、`staged` 进入 checkpoint。
- ~~原任务 1.2：落实渲染位置规则与缓存不变式~~
  - 验收：中途激活 skill 后 `PrefixFingerprint.common_turn_prefix` 不缩短，在 boundary 处提升成功；修正 `partitions.rs:117` 的注释和 `docs/concepts/prompt-cache-design.md`。
  - 文件：`context/manager.rs`、`renderer.rs`、`partitions.rs`

### 阶段 2：生命周期策略下沉 kernel（修 Q2、Q9）

> 前置：当前分支 `feat/xiaoman-memory-policy-v2` 上未提交的 recall 改动正是本阶段要删的宿主侧逻辑。先决定合入还是放弃，再开工，避免两边同时改。

- [x] 任务 2.1（2026-11-02）：`memory_policy.recall` 打开后，kernel 在 run 开始和每次 renewal 后以目标为查询发出 `query_memory`（causation `kernel_recall`），命中写成 `memory:<record_id>` knowledge 索引行（去重在 kernel）。四个 SDK 删除 prefetch 和 `memoryRecallTurn`，宿主只执行检索，`preQueryMemory` 保留为查询改写入口。未做：`task_state.plan` 变更时召回；子节点由自己的 run 开始召回覆盖。
- ~~原任务 2.1：recall 移入 kernel，并增加召回时机~~
  - 验收：kernel 在 turn 1、renewal、`task_state.plan` 变更、子节点 spawn 四个时机发出 `memory_query` effect（查询词取当前目标或子目标）；宿主只执行 query 并回填结果。index line 生成、按 `memory:<record_id>` 去重、放置、lease 都由 kernel 决定。删除四个 runner 的 `prefetchMemory` 和三个 `recall-projection.*`。`preQueryMemory` hook 保留为查询改写入口。
  - 验证：recall 测试向量在 kernel 单测中跑；SDK 测试只覆盖 I/O。
  - 文件：新增 `context/recall.rs`

- [x] 任务 2.2（2026-11-01）：kernel 按节点的 `DependencyContext { mode: full | reference | none, max_tokens }` 生成依赖段，随 spawn 以 `dependency_brief` 下发；删除三个 SDK 的 `workflow-control-flow.*`、`summary` 模式和从未生效的 `include`。Rust SDK 由此第一次带上依赖输出。预算说明仍在宿主（依赖宿主侧剩余预算）。
- ~~原任务 2.2：子节点 brief 由 kernel 生成~~
  - 验收：goal、依赖输出、预算说明、loop 指令由 kernel 生成为 item 列表随 spawn 下发；删除 Node/Python/WASM 的 `workflow-control-flow.*`（Rust SDK 没有这个文件）。
  - 文件：`orchestration/workflow/run.rs`（`spawn_info`）

### 阶段 3：DAG 边一等化（修 Q6、Q7）

- [x] 任务 3.1（2026-11-02）：子 run 的 kernel 通过 `initial_context.dependencies` 持有依赖输出，`read_result(call_id="dep:<agent_id>")` 由 kernel 直接作答（无宿主 effect，不依赖 payload store）。brief 截断时注明读取方式；新增 `handle` 模式只给 600 字符预览。四个 SDK 把 spawn 的 `dependency_outputs` 传进子 run（Rust SDK 只有子 run 一侧，其 workflow 编排属于 6.3）。未做：按 output schema 的 `fields` 结构化切片。默认模式已改为 `handle`（2026-11-02，依据 4.3 第二步重跑）。
- ~~原任务 3.1：`EdgeSpec` 与节点结构化输出~~
  - 验收：`depends_on` 带 `{ mode: full | handle, max_tokens, fields? }`，取代现有的 `full | summary | reference`。节点输出存为 handle item（`scope = edge`）。默认 `handle`：下游只看到 preview，通过 `read_dependency(id, range|field)` 调入全文。`fields` 配合节点的 output schema 给出结构化切片。预算全部按 token 计。删除 `summarizeDependency` 和 `summary` 模式：需要压缩时由上游节点自己按 output schema 产出精简结构，不在边上另做摘要。
  - 验证：fan-out(5)→synthesize 时，下游初始上下文不超过 Σ preview；page-in 能取到完整输出。
  - 文件：`orchestration/workflow/mod.rs`、`run.rs`、`mm/handle.rs`；SDK 侧新增 `read_dependency` meta-tool

- [x] 任务 3.2（2026-11-01）：Plan/Implement 默认改为 SystemOnly（kernel 两处默认表 + 三个 SDK 模板）。未做：开启 Full 时记录 token 代价。
- ~~原任务 3.2：修正继承默认值~~
  - 验收：Plan/Implement 默认改为 `SystemOnly + brief`（brief 来自父 agent 的 task_state 与显式 handoff）；`Full` 只能显式开启，开启时记录 token 代价。
  - 验证：默认 workflow 的子节点初始 token 显著下降，阶段 4 的基准上成功率不降。
  - 文件：`types/agent.rs:169`、`proc/mod.rs`

### 阶段 4：反馈闭环（修 Q4、Q10）

- [x] 任务 4.1（2026-11-01，汇总部分）：kernel 在 run 结束时发出 `lifecycle_report` 观测（knowledge 条目数、被引用数、regret、waste_tokens、cache_loss_tokens），四个 SDK 投影为会话事件 `context_lifecycle_reported`；evicted key、regret、waste 进入 checkpoint。逐 item 明细与 recall 命中率（2026-11-02，随 1.1a）：`lifecycle_report` 增加 `recalled`、`recall_expanded` 和 `items`（每个 knowledge item 的 key、origin、tokens、注入 turn、引用次数、是否 expanded、是否淘汰后回来、离开原因 evicted / expired / removed），四个 SDK 原样投影到 `context_lifecycle_reported`。history item 的明细随 1.1b。
- ~~原任务 4.1：lifecycle report~~
  - 验收：每个 run 输出每个 item 的注入、引用、淘汰、page-in、到期记录，以及三个指标：
    - **regret**：淘汰或折叠后又被 page-in 或重新请求的 item 数；
    - **waste**：从注入到消亡都没被引用的 token 数；
    - **cache_loss**：因改写 prefix 损失的缓存 token 数。
    recall 的命中率（被 `memory(record_id)` 展开的比例）和 waste 也在这份报告里统计。
  - 文件：新增 `context/lifecycle_report.rs`；SDK 的 `on_turn_metrics` 增加汇总

- [x] 任务 4.2（2026-11-03，部分）：引用证据分三级——强（`memory(record_id)`、tool 参数中出现 key 或记录 id、正文点名 key）与词法（只算特征词：去掉常见词与 index line 头部）；`ItemValue` 记 `strong_refs` / `lexical_refs`，保留打分强引用按 4 倍计。未做：模型版 `RelevanceJudge`（effect）——现有两个基准都不触发 knowledge 预算淘汰，lab 无法证明其价值，待有预算吃紧的场景再做。
- ~~原任务 4.2：引用证据与相关性判断~~
  - 验收：引用证据按强度分三级：模型显式调用 handle 或 `memory(record_id)` > tool call 参数里出现 item 的 key > 词法重叠。`RelevanceJudge` 有词法（兜底）和模型两种实现，模型实现是一个 effect，按批次打分，只在 pressure 超过阈值时触发；结果作为 fact 写入 `value.judged_relevance`（D1）。utility 打分和 knowledge 淘汰都读这个值。
  - 验证：在固定的长任务基准上，regret 和 waste 不高于纯词法版本，成功率不降。
  - 文件：`context/utility.rs`、`context/partitions.rs`、新增 `context/relevance.rs`

- [x] 任务 4.3（2026-11-03）：`crates/deepstrike-lab`（`report` / `gate`）与两个实测基准（`long-task.mjs` ledger-audit、`live.mjs` fan-out），trial 逐行写入，记录成功、成本、kernel 估算与同次调用的 provider 计数、lifecycle report。
  0.2.80 结果（2026-11-03，DeepSeek `deepseek-flash`，基线为 0.2.79 + memory 修复 `30e93db5`，`benchmark/context-lifecycle/results/`）：

  | 场景 | 变体 | 正确 | 输入 token / 题 | 估算误差（按请求大小加权） |
  |------|------|------|------------------|----------------------------|
  | ledger-audit | 0.2.79 → 0.2.80 | 11/12 → 11/12 | 52.4k → 56.5k | 11.6% → 0.7% |
  | fan-out full | 0.2.79 → 0.2.80 | 12/12 → 12/12 | 24.3k → 24.0k | 8.4% → 4.7% |
  | fan-out handle | 0.2.79 → 0.2.80 | 12/12 → 12/12 | 10.1k → 12.5k | 8.0% → 7.4% |

  三组 gate（正确率不降、加权估算误差 ≤10%）全部通过。ledger 压缩归档的 item 从 4.1 增到 15.5：replay（reasoning）现在计入压力，此前 kernel 少算了模型实际看到的内容。waste 从 2 升到 68 token 是计数规则变化（无关条目不再被常见词算作已引用），不与基线比较。测量教训：计量包装器若不转发 provider 的其它方法，replay 与 descriptor 都会丢失，测到的不是真实路径（已修复，早期结果作废）。
- ~~原任务 4.3：离线调参~~
  - 第一步（2026-11-02）：`benchmark/context-lifecycle/run.mjs` 用脚本化 provider 驱动真实 kernel，对比两个 checkout 渲染出的上下文（不测任务成功率）。相对基线 `5b5308d3`：长任务 prefix 复用 0.761→0.785，cache 下的 knowledge 改写 2→0，输入总量不变；fan-out→synthesize 在 `handle` 模式下首个调用 48.5k→4.5k 字符（−91%），且能用 `read_result` 读全文。3.2 的继承默认值在此场景不可见（workflow root 没有父 transcript）。任务成功率需要真实 provider 跑。
  - 第二步（2026-11-02）：`benchmark/context-lifecycle/live.mjs` 用真实模型测 fan-out→synthesize 的答题正确率。三个 explore 节点直接返回固定的 16k 字符报告（不调模型），只有 synthesize 调真实 provider；12 道题，访问码分别位于 600 字符预览内、8k 截断内、截断外。`--provider deepseek` 读 `.env` 的 `DEEPSEEK_*`（`openai` 读 `OPENAI_*`；yinli.one 的 key 返回 401，暂不可用），`--repo` 指向基线 worktree。DeepSeek `deepseek-flash` 结果（修复下文两处问题后）：

    | 变体 | 正确 | 发送字符 | 调用 | 首个调用 |
    |------|------|----------|------|----------|
    | 基线 full（`5b5308d3`） | 11/12 | 593k | 12 | 49.4k |
    | 分支 full | 12/12 | 904k | 20 | 25.1k |
    | 分支 handle | 9/12 | 699k | 26 | 31.3k |

    handle 的 3 个失败都是 DeepSeek replay 报错（见“交接”中的已知问题），不是答错。分支 full 首个调用减半，但模型会用 `read_result` 读被截断的依赖，总发送量高于基线；准确率 12/12。默认 edge mode 还不能定，需要先修 replay 问题再重跑 handle。
  - 第二步重跑（2026-11-02，修复两处 replay 问题后，每组 24 题，访问码三种深度各 8 题）：

    | 变体 | 正确 | 发送字符 | 调用 | 预览内 / 截断内 / 截断外（发送字符） |
    |------|------|----------|------|------------------------------------|
    | 基线 full（`5b5308d3`） | 24/24 | 1185k | 24 | 395k / 395k / 395k |
    | 分支 full | 24/24 | 1861k | 41 | 254k / 918k / 689k |
    | 分支 handle | 24/24 | 1394k | 51 | 560k / 503k / 331k |

    replay 问题有两个来源：一是 kernel 折叠旧叙述后按内容查 reasoning 查不到（已修，见“交接”）；二是 `deepseek-flash` 确实会在带 tool call 的轮次不给 reasoning（本次 51 次调用中 4 次），DeepSeek 接受回放空的 `reasoning_content`、拒绝缺失，因此改为原样回放空串。三组准确率持平。分支 full 比基线贵 57%：8k 截断后模型几乎总会 `read_result` 读全文；handle 比分支 full 省 25%，但答案在预览内时模型仍会去读全文。基线不截断，在 16k 输出的场景最省，但下游初始上下文随 fan-out 宽度线性增长。
  - 验收：`crates/deepstrike-lab` 提供工具，用 lifecycle report 和任务结果（成功/失败、成本）比较不同 `ContextPolicy`（阈值、lease 默认值、edge mode）。不依赖已删除的 evolution 模块。
  - 文件：`crates/deepstrike-lab`、`benchmark/`

### 阶段 5：统一上下文权威（修 Q5）

- [x] 任务 5.1（2026-11-03，kernel 侧）：消息 ABI 加 `item_id` 与不透明 `replay`（kernel 随 item 保存 reasoning / native blocks 并渲染回去，四端 SDK 发送与优先使用）；渲染请求带 `estimated_tokens`（含 tool schema 与 replay，按最近 8 次估算/实测做最小二乘校准，只抬高不压低压力）；`ProviderCompleted` 加 `provider_compaction`（覆盖到某 item 的摘要换成一个 `provider_summary` item）与 `cleared_tool_results`（渲染为 handle），四端 SDK 经可选的 `takeContextEdits` 透传。未做：Anthropic provider 上报这两类 fact（无 Anthropic key，无法实测三种 compaction 流程）。
- ~~原任务 5.1：吸收 provider 对上下文的改写~~
  - 核实（2026-11-01）：kernel 消息 ABI 只有纯文本，compaction/thinking 等原生 block 留在宿主 provider 内，所以“用 provider_summary item 替换被覆盖的 history”需要先给 `ProviderMessage`/`LogicalMessage` 加不透明 block，并改四个 SDK 的两种 compaction。只改 renewal 判据没有收益：renderer 在预算外丢最旧 history，与 renewal 阈值（0.98）几乎同时发生。此任务依赖 1.1 和这项 ABI 扩展。
  - 验收：provider 返回 compaction block 时，kernel 收到 `ProviderCompacted{covered_range, summary_tokens}` fact，把被覆盖的 history item 替换为一个 `provider_summary` item（`authority = provider`，携带原生 block）。`clear_tool_uses` 同理，作为 fact 回填后对应 item 的 fidelity 降为 handle。之后 pressure、utility、renewal 都基于模型实际可见的内容计算。
  - 验证：开启 compaction 的长任务中，kernel 估算的输入 token 与实测 usage 偏差 <10%；renewal 不再提前触发。
  - 文件：`context/manager.rs`、`context/pressure.rs`；`node/src/providers/anthropic-compaction.ts`、`python/deepstrike/providers/anthropic_compaction.py`、`wasm/src/providers/anthropic-compaction.ts`、`rust/src/providers/anthropic.rs`

### 阶段 6：动态编排收敛（修 Q8）

- [x] 任务 6.1（2026-11-01）：核实发现 canonical ABI 从未接受 Loop/Classify/Tournament/Reduce（宿主侧直接拒绝，子结果的三个信号字段在 wire 上恒为 None），因此连同 Reduce、reducers 模块、tournament.rs 一并删除；`gen_eval` 去掉 max_iters。
- ~~原任务 6.1：删除声明式控制流节点~~
  - 验收：删除 `NodeKind::Loop/Classify/Tournament`、`orchestration/tournament.rs` 及 scheduler 中的 re-arm、prune、bracket 逻辑；kernel 只剩 `Spawn` 和 `Reduce`。Classify 改用模型运行时提交（`mod.rs:86` 的注释已推荐这种写法），Loop 和 Tournament 改用宿主程序。迁移文档给出三种节点的改写示例，不提供同名模板。
  - 文件：`orchestration/workflow/mod.rs`、`run.rs`、`scheduler/state_machine/workflow.rs`

- [x] 任务 6.2（2026-11-03）：`ctx.blackboard.put/get/delete`；条目为操作上的 `blackboard:<key>` item（origin `blackboard`，scope `workflow`：此后派生的 agent 不论 context inheritance 都继承），phase 级条目在 phase 结束时移除。四端 SDK 均支持。
- ~~原任务 6.2：编排程序的共享状态写成 item~~
  - 验收：跨 phase 共享的数据通过 `blackboard.put(key, value, {scope, lease})` 写成 item，子 agent 按 scope 可见；不再靠闭包变量拼进 prompt。
  - 文件：`node/src/workflow/dynamic.ts`

- [x] 任务 6.3（2026-11-03）：Python `run_dynamic_workflow`、WASM `runDynamicWorkflow`、Rust `run_dynamic_workflow`，与 Node 词汇一致；Rust 的 workflow 驱动循环抽成 `drive_workflow` 并补上子 run 的 `inherited_knowledge`。已保存运行的重放与沙箱脚本执行器仍只有 Node。
- ~~原任务 6.3：补齐 SDK~~
  - 验收：宿主程序 API 按 Python、Rust、WASM 的顺序移植，接口与 Node 一致。重放沿用 `DynamicWorkflowReplayFact` 作为已记录输入（D1），不需要跨 SDK 字节一致。

### 阶段 7：item 图（0.2.81）

**为什么。** item 之间本来就有带方向的关系，现在都藏在位置、计数或 key 约定里，没有作为一等的边存下来：

| 关系（方向） | 现在的表示 | 缺口 |
|---|---|---|
| tool result → 它回答的 call（`answers`） | 按消息位置推断（`units::unit_boundaries`） | 压缩、渲染、renewal、provider 摘要覆盖都靠位置保证"不拆交易" |
| history item → 它引用的 knowledge（`references{grade}`） | 只在被引用的条目上计数（`strong_refs` / `lexical_refs`） | 不知道是哪条消息引用了它，regret / waste 无法归因 |
| 读取 → 被读回的 item（`reads_back`：`read_result`、`memory(record_id)`） | 按 origin 汇总的次数 | 读回了谁、为什么读回都丢了 |
| provider_summary / 压缩摘要 → 被覆盖的 items（`summarizes`） | 被覆盖的 item 直接退场 | 摘要的来源不可查，无法按需取回 |
| 子 run 继承的副本 → 父 item（`inherited_from`）；下游 → 依赖输出（`depends_on`） | 没有记录；DAG 只在工作流层 | 跨 run 的数据流与 item 断开 |

**模型。** 每个 item（history、knowledge、provider_summary）都有 `ItemId`。边是 `ItemEdge { from, to, relation, turn }`，relation 是一个固定的小集合，不做通用图库。约束：
- 边只由已记录的输入产生（D1），重放得到同一张图；
- 边进 checkpoint，数量有上限：两端都在场的边全部保留，指向已退场 item 的边只保留最近 `HISTORY_ITEM_DETAIL_CAP` 条；
- 先只上报不改行为，lab 证明有收益后再让边参与决策（与 0.2.80 相同的 lab 先行顺序）。

- [x] 任务 7.0（2026-11-02）：`benchmark/context-lifecycle/knowledge-pressure.mjs`：账本审计的两条规则只在 memory 里，另有十条干扰记录（多条同样涉及 audit / ledger），knowledge 预算 4%，窗口 7000。trial 带 `oracle`（场景知道哪些条目是必需的）：`required_dropped`、`required_unreferenced`、`distractor_refs`、`distractor_resident_tokens`，lab 以 `oracle.<name>` 报告和 gate。0.2.80 基线（DeepSeek，12 题，`results/knowledge.jsonl`）：正确 9/12，regret 0，waste_tokens 24，读回 0.67；必需规则从未被淘汰、始终被引用；干扰项平均被记 33 次引用，结束时仍有 246 token 在场。结论：在这个预算下 0.2.80 不丢必需条目，缺口在证据：词法重叠把干扰项也算作"用过"，kernel 自报的 waste（24）远低于真实的无用驻留（246）。7.3 的目标据此改为降低 `oracle.distractor_refs` / `oracle.distractor_resident_tokens`，同时 `required_dropped` 保持 0、正确率不降。
- ~~原任务 7.0：基准先行~~
  - 验收：新增 `knowledge-pressure` 场景：knowledge 预算偏紧，种入大量无关条目和少量答题必需的条目（含 memory recall），答案依赖必需条目；在 0.2.80 上跑出基线（成功率、regret、waste、读回次数）。现有 ledger / fan-out 场景照常作为回归。
  - 文件：`benchmark/context-lifecycle/`

- [x] 任务 7.1（2026-11-02）：knowledge item 与 history 共用一个 id 分配器（旧 checkpoint 的 slot 恢复时按顺序补发）；新增 `context/graph.rs`，边表进 checkpoint（`item_edges`），两端在场的边全留、其余只留最近 128 条。记录的边：`answers`、`references_strong` / `references_lexical`、`reads_back`（发出 `read_result` / `memory(record_id)` 调用的 assistant item；memory 搜索命中不算读回 → 被读回的 item）、`summarizes`（provider 摘要 → 被覆盖的 item）。lifecycle report 增加 `edges`、`edges_omitted`、`waste_items`，item 记录带 `id`；四端 SDK 投影与 contract 已同步。golden 只有 checkpoint / report 字段和摘要值变化。与验收的出入：kernel 自己的压缩不产生摘要 item，所以 `summarizes` 只来自 provider 摘要；regret 的条目由 kernel recall 重新放入，没有发起读取的 item，因此 regret 用 item 记录的 `id` + `paged_in_after_evict` 加上指向它的边来定位，而不是"某条读回边"。
- ~~原任务 7.1：所有 item 都有 id，边表只上报~~
  - 验收：knowledge item 从同一个分配器取 `ItemId`（checkpoint 携带，缺 id 的旧 slot 恢复时补发）。manager 持有边表，产生 `answers`（tool result → call 所在的 assistant item）、`references{strong|lexical}`（history → knowledge，取代只计数）、`reads_back`（`read_result` / `memory(record_id)` → 目标）、`summarizes`（摘要 → 被覆盖的 item）。lifecycle report 增加有上限的 `edges`，regret 记录是哪条读回边把被淘汰的 item 拉回来的，waste 列出没有任何入边的 knowledge。渲染与压缩不变：golden 只在 checkpoint / report 字段上变化。
  - 文件：`context/item.rs`（`ItemEdge`、`Relation`）、新增 `context/graph.rs`、`context/manager.rs`、`context/partitions.rs`、`runtime/kernel/wire/checkpoint.rs`；session event 注册表与四端 SDK 投影

- [x] 任务 7.2（2026-11-02 决定不做）：`unit_boundaries` 本来就按 `call_id` 配对 tool result 与 call，不依赖位置；它还把用户消息和收尾回答并入同一单元，这是 `answers` 边表达不了的。改成按边的连通分量既拿不到新信息，又做不到渲染逐字节不变。`answers` 边保留在报告里，供归因使用。
- ~~原任务 7.2：交易单元来自 `answers` 边~~
  - 验收：压缩、renderer、renewal、provider 摘要覆盖范围、held history 都按 `answers` 边的连通分量确定"不可拆开的单元"，删除按位置推断的 `unit_boundaries`（或降为恢复旧 checkpoint 的兜底）。
  - 验证：随机 transcript 的配对性质测试照常通过；现有 golden 的渲染结果逐字节不变。

- [x] 任务 7.3（2026-11-02）：证据按来源计：词法重叠只在模型自己的话和用户消息里算"用过"，工具输出只有点名（key / record id）才算，memory 命中（记录原文）不算；新增 `expands` 边（memory 命中 → 它展开的 recall），模型用到命中内容时，使用沿这条边传一跳记到 recall 上；被在场 history item 强引用或读回的条目不过期、不被预算淘汰。结果（DeepSeek，12 题，对 0.2.80）：`knowledge-pressure` gate 通过，干扰项引用 33 → 9.6，自报 waste 24 → 156（真实无用驻留 233），必需规则 0 丢失，正确 9/12 → 10/12；fan-out full / handle gate 通过（12/12）；ledger 首批 10/12 对 11/12 未过 gate（两题均为算术错误，规则全程在场且被引用），两边各加测 12 题后 22/24 对 20/24（0.2.80 有一题运行报错），gate 通过。与验收的出入：保留分数仍用引用计数（计数已只含有效证据），没有另算"在场入边加权"；只传一跳的传播落在 `expands` 边上。
- ~~原任务 7.3：沿边传播的保留与淘汰~~
  - 验收：item 的保留分数加上来自仍在场 item 的入边（强引用权重高于词法，只传一跳，整数权重，确定性）；knowledge 预算淘汰与 recall lease 到期都看入边——仍被在场 item 强引用的条目不淘汰、不过期。
  - 验证：`deepstrike-lab gate` 在 `knowledge-pressure` 上 regret、waste 不高于 7.0 基线，成功率不降；ledger / fan-out 不回退。

- [x] 任务 7.4（2026-11-02）：派生的 `inherited_knowledge` 条目带 `inherited_from { operation, item }`，`KnowledgeEntry` 接受这个字段，四端 SDK 原样传给子 run；子 run 记 `inherited_from` 外部边，`read_result("dep:<agent>")` 记 `depends_on` 外部边（读取方的 assistant item → 依赖 agent）。lifecycle report 增加 `external_edges` 与 `parent_items_used`（子 run 用到的父 item）。外部边进 checkpoint（`item_external_edges`），最多保留最近 128 条。顺带修复 Python 起始上下文丢 `pinned`。
- ~~原任务 7.4：跨 run 的边（只上报）~~
  - 验收：继承下发的 knowledge 带 `inherited_from { operation, item }`，子 run 的 item 记录这条边；依赖输出读回记为 `depends_on` / `reads_back`。子 run 的 lifecycle report 能指出父 run 的哪些 item 被用到。
  - 文件：`orchestration/workflow`、`runtime/kernel/wire/driver/continuation.rs`、四端 SDK 的 `inherited_knowledge`

### 阶段 8：测量先行，缓存进入决策（0.2.82）

**为什么。** 阶段 0–7 把账本建完了，但 0.2.79–0.2.81 的实测数据说明，瓶颈已经不是缺机制，而是账算得不对、决策没有考虑缓存成本（`results/ledger.jsonl`，DeepSeek `deepseek-flash`；未命中输入 = 输入 − 0.9 × 缓存读取）：

| 版本 | 正确 | 输入 token / 题 | 缓存命中率 | 未命中输入 / 题 | kernel 自报 `cache_loss_tokens` |
|------|------|-----------------|------------|-----------------|--------------------------------|
| 0.2.79 | 11/12 | 52.4k | 63% | 22.7k | 0 |
| 0.2.80 | 20/24 | 57.1k | 53% | 29.6k | 0 |
| 0.2.81 | 22/24 | 54.4k | 56% | 27.1k | 0 |

| # | 问题 | 证据 |
|---|------|------|
| Q12 | `cache_loss` 只统计 knowledge 提升（`manager.rs` 的 `cache_invalidated_tokens`），压缩、叙述折叠、预算外丢弃、renewal 造成的 prefix 改写都没计入；kernel 报 0，而实测命中率从 63% 降到 56%。与 7.0 发现的 waste 自报偏低是同一类问题 | 上表 |
| Q13 | 渲染时每一轮都会改写 prefix 中段：`collapse_assistant_narration` 默认开启，`preserve_recent_turns` = 2，刚滑出保护窗口的叙述单元在这一轮被替换成 `NARRATION_STUB`；超出预算时，最旧的单元按轮依次被丢弃。两者都让断点之后的内容每一轮重新按未命中计费 | `renderer.rs` 的 `project_assistant_narration`、render 循环 |
| Q14 | 基准规模太小，gate 区分不了改进和噪声：窗口 7–8k，峰值上下文不到 10k，每组 12 题；9/12 对 10/12 落在噪声里。100k 以上窗口、几十轮以上的长任务没有覆盖，因此"`RuleSummarizer` 是否是瓶颈"（见“范围外”）的数据永远不会出现 | `long-task.mjs` 默认 `--max-tokens 8000`；`knowledge-pressure.mjs` 窗口 7000 |
| Q15 | gate 以 token 数和字符数为主，没有按价格加权；token 少但缓存命中降低的改动会被当成改进 | `crates/deepstrike-lab` 的 `Summary` |
| Q16 | 成功指标里的“输入 −25%、cache_loss −50%”没有达成，而现有 gate 发现不了这一点 | ledger 输入 52.4k → 54.4k |

**原则。**
- 与 0.2.80、0.2.81 相同，lab 先于被衡量的改动：8.0–8.2 只改测量，不改渲染；8.3、8.4 改变决策，必须先过新的 gate。
- 缓存损失和 waste 一样，以 provider 实测为准，kernel 自报的数字必须能与实测对上。
- 冻结图的扩张：本阶段不新增 relation、lease 或 origin。已有的边只有在 lab 证明收益后才参与决策。

- [x] 任务 8.0（2026-11-11）：新增 `context/cache_ledger.rs`。每次请求发出时按段（工具 schema、system[0]、system[1]、每个 history turn）取指纹并与上一次请求比较，第一个不同的段即断点。原因由造成改写的动作登记（`snip` / `micro` / `collapse` / `auto` / `renewal` / `provider` / `promotion`），未登记的从渲染推断（`toolset` / `system` / `narration` / `budget_drop` / `reprojection` / `other`）。lifecycle report 增加 `prefix_breaks`（最近 128 条）、`prefix_breaks_omitted`、`cache_tail_tokens`，`cache_loss_tokens` 改为所有断点之和；账本进 checkpoint。lab 读 trial 的 `cost.requests[]`，报告 `observed_cache_loss_tokens`、`cache_loss_error`、`cache_loss.<cause>`，并支持 `gate --max-cache-loss-error`。四端 SDK 投影与 contract 已同步，Rust workspace、Node、Python、WASM、四端 conformance 全部通过。脚本化长任务（`run.mjs`，25 次调用）记录到两次 `micro` 断点，共 17.6k token，改动前该值报 0。与验收的出入：
    - 字段名用 `requests[]`，不用 `calls[]`（`cost.calls` 已是调用次数）；断点记为 `cause` 加 `kept_tokens` / `lost_tokens`（断点位置以 token 计）。
    - 原因比原计划多 `toolset`、`system`、`reprojection`、`other`：skill 收窄工具集会让整个缓存失效，这是原计划没有列出的一类断点。
    - 新增 `cache_tail_tokens`：上一次请求的 state turn 每次都会重建，本来就不可复用，但 provider 统计的未命中输入里包含它；lab 先从实测值中扣除这部分，再与 kernel 自报值比较。
    - 指纹只覆盖发给 provider 的内容：tool result 的 `durable_content` 不计入（恢复后的消息带这个字段，实时消息可能没有，否则恢复后会多出一个假断点）。
    - 新发现的每轮断点来源（留给 8.3）：压力超过 collapse 阈值后，`recompute_handle_residency` 每轮会再折叠一个旧工具结果。
    - 实测（DeepSeek `deepseek-flash`，各 12 题，`0.2.82-8.0`）：kernel 自报与实测缓存损失偏差 ledger 11.6%、knowledge-pressure 7.3%，验收通过。ledger 每题损失按原因：collapse 7.3k、toolset 5.9k、narration 5.6k。正确率 10/12、8/12，因为 8.0 不改渲染，这是噪声，也正说明 8.1 的配对规则有必要。
  - 验收：trial 的 `cost` 增加逐次调用明细 `calls[]`（`input_tokens`、`cache_read_tokens`、`cache_write_tokens`、`output_tokens`，以及 kernel 给出的 prefix 断点）。lab 计算实测缓存损失：每次调用 `max(0, 可复用前缀 − cache_read)`，其中可复用前缀为上一次调用的输入去掉本轮新增的尾部。kernel 在 lifecycle report 中增加 `prefix_breaks`（turn、动作 `promotion | snip | micro | collapse | auto | narration | budget_drop | renewal | provider`、断点位置、断点之后的 token 数），`cache_loss_tokens` 改为所有断点之和，不再只算 knowledge 提升。
  - 验证：在 ledger、knowledge-pressure 上，kernel 自报的 `cache_loss_tokens` 与实测缓存损失的偏差 ≤ 15%（容差来自 provider 的缓存粒度，例如 DeepSeek 的 64 token 块）。
  - 文件：`context/manager.rs`、`context/renderer.rs`（渲染时返回断点）、`crates/deepstrike-lab`、`benchmark/context-lifecycle/*.mjs` 的计量包装器；四端 SDK 投影与 contract

- [x] 任务 8.1（2026-11-11）：lab 增加价格表（`--prices`，默认是相对单位：未命中输入 1、缓存读取 0.1、缓存写入 1、输出 4；`benchmark/context-lifecycle/prices.json` 只放默认值，真实价格由使用者填写）与 `weighted_cost`（报告第一行）；`gate --paired metric:tolerance` 按 (场景, 题目) 配对，固定种子 bootstrap 给出 95% 区间，少于 `--min-pairs`（默认 30）报“样本不足”。trial 增加 `model` 与 `cache_write_tokens`。重算现有结果：ledger 0.2.79 → 0.2.81 的 `weighted_cost` +11.6%，而 `input_tokens` 只有 +3.8%，验证通过。与验收的出入：
    - 正确率容差 5 个百分点在 30–40 对样本下不可用：两边各错一题、净差为 0 时，区间就有 ±7.5 个百分点。推荐用 `success_rate:0.10`，它能容忍相互抵消的波动，净输 3 题时仍然失败（有测试）。
  - 验收：lab 增加价格表（按 provider / model：未命中输入、缓存读取、缓存写入、输出单价），新增指标 `weighted_cost`，并设为成本类 gate 的主指标；`input_tokens` / `sent_chars` 降为诊断指标。gate 改为同一题在两个变体上配对比较，用 bootstrap 给出差值的 95% 置信区间：正确率差值的下界不低于 −5 个百分点才算“不降”；样本少于 30 对时 gate 报告“样本不足”，不再给出通过。
  - 验证：用现有三份 results 重算，0.2.79 → 0.2.81 的 ledger 在 `weighted_cost` 上应显示回退（上表的未命中输入 +19%）。
  - 文件：`crates/deepstrike-lab/src/lib.rs`、`main.rs`

- [x] 任务 8.2（2026-11-11）：`long-horizon.mjs`：事故调查任务。评分规则只写在开头读的 `brief.md` 里（要数的错误码、要跳过的请求 id、只在最后用到的倍数和值班徽章号），之后读 40 个各约 3.5k token 的日志，窗口 100k。`--dry` 不调模型、只打印题目事实，`--rescore` 从已记录的答案重算 oracle，`--concurrency` 并行。基线（DeepSeek，30 题，8.3 之前的代码）：正确 12/30，平均 40 次调用、170 万输入 token、压缩 5.6 次；kernel 自报缓存损失与实测偏差 4.6%；按原因 micro 164k、collapse 133k、narration 78k、toolset 9k。与验收的出入：
    - `early_fact_lost` 30 题全是 0：brief 被压缩掉后，模型会重读（平均 1.47 次）。可以重新读取的事实总能找回来，所以加长任务也让它不为 0。真正丢的是模型自己算出的中间计数：答错的题都保留了 brief 的公式（答案减去徽章号能被倍数整除），30 题里有 11 题少计。为此新增 oracle：`formula_broken`（答案不符合 brief 的公式）、`count_error`、`undercount`。
    - 模型常在一次调用里并行读多个文件，所以单题调用数为 15–40，不是每题至少 40 轮；压缩照样发生（每题 2–17 次）。
    - 渲染器的预算外丢弃（`budget_drop`）在所有场景的实测中都没有出现，8.3 不处理这一项。
  - 验收：新增 `benchmark/context-lifecycle/long-horizon.mjs`：窗口 ≥ 100k、每题 ≥ 40 轮，脚本化的工具环境返回大量输出（多账户账本或代码库排查）。答案依赖前 10 轮里出现、之后不再重复的事实，因此必然经过压缩或 renewal。oracle：`early_fact_lost`（必需事实在答题时既不在可见上下文中、也没有被读回）、`compactions`、`renewals`。在 0.2.81 上跑出基线，每个变体至少 30 题。现有三个场景照常作为回归。
  - 验证：基线运行中至少触发一次 auto 或 renewal；`early_fact_lost` 有非零基线，否则加长任务直到有。
  - 文件：`benchmark/context-lifecycle/`

- [ ] 任务 8.3（2026-11-11，三个变体均未通过 gate，未合入；候选保留在分支 `exp/8.3c`）。同一基线（`0.2.82-8.0` / `0.2.82-baseline`，每个场景 30 题）下的实测：
    - 修正计划里的公式：原文两边都乘了 (1 − 折扣)，这个因子互相约掉。正确的账是：收益 = 释放 token × 摊销调用数 × 读取折扣（被释放的 token 原本只按缓存价计费），代价 = 断点后 token × (1 − 折扣)。按默认值，中段压缩几乎永远不划算。
    - 8.3（轻量层成本闸门 + 叙述只在边界折叠 + 批量折叠工具结果 + `read_result` 一旦暴露不撤）：ledger 通过（成本 −39%，正确 23/30 → 27/30），knowledge-pressure 样本功效不足（均值有利、区间跨 0），long-horizon 成本 +20%，30 题里 6 题跑到轮数上限。闸门推迟轻量层后，运行会一路涨到硬性层，而硬性层会归档整轮对话，不是就地截短旧工具结果。模型因此丢掉自己的累计计数，重读文件和 brief（重读 1.47 → 3.23 次）。成本账只算了缓存，没算信息损失。
    - 8.3b（去掉闸门）：long-horizon 成本 −0.7%、正确 8/30（不显著），仍有 5 题跑到轮数上限。toolset 损失从 8.8k 升到 22.9k：批量折叠推迟了第一次换出，`read_result` 在前缀最大时才第一次出现。
    - 跑到轮数上限的那些运行，结尾都在把 DeepSeek 的工具调用标记当作普通文本输出。旧叙述保留原文直到边界，这类泄漏的标记就会在上下文里停留多轮。两个变体合计 60 题里 11 题跑到上限，基线 30 题里只有 1 题。
    - 8.3c（恢复每轮折叠叙述，`read_result` 从第一个工具结果起就提供并保持）：实测时 provider 账户余额耗尽，ledger 16/30、long-horizon 10/30 以 `402 Insufficient Balance` 中断，数据无效，移到 `results/partial-8.3c.jsonl`。
    - 结论：本版本默认渲染行为回到 8.0；代码见 `exp/8.3c`（`c8240078`）。下次实测从 8.3c 开始，long-horizon 与 ledger 各 30 题。
    - 已排除的方向：轻量层的成本闸门（以信息损失换缓存）、叙述只在边界折叠（循环风险）。
    - 8.3c 重测（2026-11-11）：ledger 30 题通过配对 gate（正确 23/30 → 26/30，`weighted_cost` −29.7%，缓存损失 −39.5%）；long-horizon 跑到 13/30 时决定停止（13 题中 3 题正确），结果移到 `results/partial-8.3c-horizon-stopped.jsonl`。
    - 放弃（2026-11-11）：上下文管理转向阶段 9，不再按缓存账决定压缩什么；`exp/8.3c` 只作参考。
  - 验收：压缩与渲染投影都先算账再执行：一个会产生 prefix 断点的动作，只有在“释放的 token × 摊销调用数 × (1 − 缓存读取折扣)”大于“断点之后的 token × (1 − 缓存读取折扣)”时才执行。`ContextPolicy` 增加 `cache_read_discount_ppm`（默认 100_000）和 `amortize_calls`（默认 8）；窗口硬上限优先于此规则，超出上限时照常压缩。具体改动：
    - 叙述折叠不再在每轮渲染时逐个执行，改为在 boundary 或压缩时批量执行（修 Q13）；
    - 预算外丢弃最旧单元时，一次丢到 `target_after_compress`，而不是每轮丢一个；
    - 一次压缩尽量合并成一个断点：所有改写从同一个最早位置开始，不在多个位置各断一次。
  - 验证：`deepstrike-lab gate` 在 ledger、knowledge-pressure、long-horizon 上，`weighted_cost` 下降、正确率不降（8.1 的配对规则）；fan-out 不回退。golden 会变化，需要重新 bless 并在 MIGRATION 中说明。
  - 文件：`context/compression.rs`、`context/renderer.rs`、`context/pressure.rs`、`context/policy.rs`、`context/config.rs`

- [ ] 任务 8.4（2026-11-11，已实现并实测，未通过 gate，已撤回，不随 0.2.82 发布；代码保留在分支 `exp/8.4-model-summaries`）：
    - 前置数据：long-horizon 四个变体共 120 题里，`early_fact_lost` 只在 8.3b、8.3c 出现过 5 次；答错的主要原因是计数偏差，而且与压缩次数不相关。按前置条件本不该做，按决定先实现再实测。
    - 实现：ContextCollapse / AutoCompact 归档之后，kernel 发出 `summarize_context` effect（被归档的对话 + 上一份摘要，原文上限 16k token，摘要上限 800 token），宿主用自己的模型写摘要，替换规则摘要；失败、超时或为空时保留规则摘要。新增 `summary` origin 与 `summarizes` 边，四端 SDK 的选项 `modelSummaries` / `model_summaries`，默认关闭。
    - 实测（long-horizon 30 题，`0.2.82-8.4` 对 `0.2.82-baseline`，配对）：正确 12/30 → 4/30（−26.7 pt，95% 区间 [−46.7, −3.3]），`weighted_cost` +71%（[+39%, +102%]），两项都不通过。10 题跑到轮数上限（基线 1 题），结尾都在以文本输出 DeepSeek 的工具调用标记；平均压缩 5.6 → 9.8 次；brief 重读 1.47 → 0.77 次，`early_fact_lost` 仍为 0。没陷入循环的错题，仍是差 1–3 的计数偏差。
    - 判断：模型摘要保住了规则，但换不来正确率；同步摘要放在窗口最紧的时刻，和任务争窗口。错因是计数偏差和标记泄漏，压缩管线管不到。
    - 结论：撤回，不扩 effect ABI。`summarize_context` 的管道在阶段 9 改作边界上的整理 effect（任务 9.3）。
    - 另：`deepstrike-lab gate` 不带 `--paired` 时对 8.4 报 PASS，默认 gate 不比较正确率与成本（见任务 9.0）。
  - 前置条件：long-horizon 基线中，`early_fact_lost` 主要发生在 `RuleSummarizer` 摘要覆盖的范围内。否则此任务不做，并记录数据。
  - 验收：auto 压缩时，kernel 发出 `summarize` effect（输入为要归档的 item 范围，带 token 上限），宿主调用模型，摘要作为已记录的 fact 回到 kernel（D1，重放得到同一张图），写成一个 `summary` item，带 `summarizes` 边指向被覆盖的 item。effect 失败或超时时退回 `RuleSummarizer`。`summarizes` 边从此也来自 kernel 自己的压缩，被覆盖的原文可以沿这条边读回。
  - 验证：long-horizon 上 `early_fact_lost` 下降、正确率不降，`weighted_cost`（含摘要调用本身）不升。
  - 文件：`context/summarizer.rs`、`context/compression.rs`、`runtime/kernel/wire/effect.rs`；四端 SDK 的 effect 执行

暂缓项保持暂缓：模型版 `RelevanceJudge` 与 Anthropic 上报 compaction fact，等 long-horizon 能衡量它们时再排期。

### 阶段 9：模型构建、修复、索引上下文图（0.2.83 起）

**为什么。**
- 第 8 章两次改变决策的尝试都没过 gate，共同点是由 kernel 按规则判断内容的意义：8.3 按缓存账决定压什么，8.4 在压缩时同步生成摘要决定留什么（之前是 `RuleSummarizer` 按关键词分槽）。long-horizon 的主要错因（模型自己记的累计计数丢失、少计）和最大的单一失败来源（DeepSeek 把工具调用标记当文本输出，8.3、8.3b、8.4 合计 21/90 题跑到轮数上限，基线 1/30）都不是压缩规则能解决的。
- 参考：Anthropic 关于 agent 记忆的研究。四层结构：transcripts（底账）、模型在任务中写的 notes、离线整理成的 wiki 式表示、每次会话注入的轻量索引；离线整理（"dreaming"）对照 transcripts 修正错误笔记、修复死链、建索引；"只记失败"这类限制性指令让强模型的收益下降，方向性指令让弱模型的收益明显上升；wiki 加检索优于只有检索。我们读到的是图表和二手解读，设计据此，具体数字以原文为准。
- 我们已有其中两层的骨架：item 表加归档（即 transcripts：每条有 id，可用 `read_result` 读回），item 图（边由已提交输入记录，可重放）。缺的是：图里没有模型写的节点，决策也不读图（7.1 起图只上报）。

**原则。**
- 分工：模型决定意义（记什么、怎么链接、哪条错了、怎么索引）；kernel 决定机制（预算、渲染位置、缓存、来源、校验、重放）。kernel 不再生成代替模型理解的内容：不新增规则摘要的语义槽，不按缓存账删除内容。
- 证据层只增不改：原始 item 与归档是唯一的底账。模型写的节点必须引用证据（`derived_from` 边），修复时以证据为准。
- D1 不变：会话内的笔记来自模型的工具调用（已提交输入）；整理走 effect，结果作为 fact 记录，重放得到同一张图。
- 诊断由 kernel 算，修复由模型做：死链、无证据的笔记、证据已有更新的笔记、近似重复、从未被读的笔记，由 kernel 确定地算出，作为整理的输入，模型不必扫描全图。
- 指令给方向，不给禁令；指令文本作为实验变量，在 lab 中比较。
- 本阶段解冻“范围外”里的记忆写入冻结：跨会话写入沿用已有的 candidate ABI（`propose_memory`、attestation、lease），不另建写入通道。
- 每项改动都要过 8.1 的配对 gate（`--paired success_rate:0.05,weighted_cost:0`），对照 9.0 的新基线。

**新增的节点与边。**

| 名称 | 种类 | 说明 |
|------|------|------|
| `note` | origin | 模型写的节点：`kind`、`title`、`body`、`status`（active / retired）。`kind` 取 `state`（会话内的工作状态，如累计计数、已处理清单，只活到会话结束）、`rule`（隐性规则或经验，可提升为记忆）、`question`（未决问题，回答后退役）、`index`（索引页，可提升为记忆） |
| `derived_from` | relation | note → 证据 item（工具结果、用户消息、归档条目） |
| `links_to` | relation | note → note，wiki 式链接；目标可以尚不存在（死链），由整理修复 |
| `supersedes` | relation | 新 note → 被修正或合并的旧 note，旧 note 退役但保留 |

- [ ] 任务 9.0：先清掉测量里的混杂因素
  - DeepSeek 以文本输出工具调用标记（`｜DSML｜`）：在 OpenAI-chat 方言层识别，能解析就转成工具调用，不能解析就作为 provider 协议错误交给 kernel 已有的恢复路径，不让它以普通文本进入 history。
  - 新基线：在 0.2.82 上重跑 long-horizon 30 题（`0.2.83-baseline`），之后的 gate 都对照它。
  - lab：`gate` 不带 `--paired` 时默认按 `success_rate:0.05,weighted_cost:0` 比较，避免 8.4 那样的假 PASS。
  - 验证：long-horizon 跑到轮数上限的题 ≤ 1/30；四端各有一个标记泄漏的单测。
  - 文件：四端 OpenAI-chat provider 与方言表、`crates/deepstrike-lab`
- [ ] 任务 9.1：会话内笔记（模型构建）
  - kernel 工具 `note`：`write`（必须带 `evidence`，即 call_id 或 item id 列表）、`update`、`link`、`retire`。`update_plan` 只保留计划步骤，`progress` 自由文本由 `state` 笔记取代。
  - 笔记属于 working 分区，不参与压缩；总量有上限 `notes_budget_tokens`。超出上限时 kernel 不替模型删除，而是拒绝写入，并提示模型合并或退役，由模型决定删什么。
  - 渲染：active 的 `state` 与 `question` 笔记渲染进 state turn（每轮重建，不进缓存前缀）；`rule` 与 `index` 在 boundary 提升进 system[1]，沿用缓存不变式。
  - 指令：工具说明给方向，例如“记录你推导出来、之后无法从文件重新得到的东西（计数、清单、结论），并注明依据”，不写禁令。
  - 验收：long-horizon 上 `undercount` 与 `count_error` 下降，正确率高于 `0.2.83-baseline`（配对），`weighted_cost` 不升；ledger 不回退。
  - 文件：`context/item.rs`、`context/task_state.rs`、`context/manager.rs`、`context/renderer.rs`；四端 SDK 的事件投影与 contract
- [ ] 任务 9.2：压缩读图（证据换出，笔记留下）
  - 选择换出单元时读图：被 active 笔记 `derived_from` 引用的证据优先换出（意义已在笔记里，原文可沿边读回）；没有被任何笔记引用、且仍在当前工作范围内的证据视为“未处理”，后换出。
  - 换出预告：要归档的范围里有未被引用的证据时，kernel 提前一轮在 state turn 里列出即将换出的条目（call_id 加一行预览），模型可以先写笔记。不额外调用模型。
  - 换出后，笔记里的证据引用渲染成可读回的句柄（`read_result call_id=…`）。
  - `RuleSummarizer` 降级为归档定位标签：只写归档了哪些 call_id、哪些文件，不再填规则、错误等语义槽。
  - 验收：同 9.1 的场景，正确率不降，`weighted_cost` 下降，`read_back` 与 `brief_rereads` 不升。
  - 文件：`context/utility.rs`、`context/compression.rs`、`context/summarizer.rs`、`context/graph.rs`
- [ ] 任务 9.3：边界整理（模型修复）
  - kernel 在 boundary（renewal、run 结束、宿主声明的空闲）算出结构诊断：死链、无证据的笔记、证据已有更新的笔记（同一文件或工具在之后有新结果）、近似重复（词法重叠超过阈值）、长期未被读的笔记。
  - 有诊断时发出 `consolidate_notes` effect：输入为相关笔记、它们的证据摘录和诊断列表；输出为编辑操作（merge、rewrite、retire、link、create index）。kernel 校验（引用的 id 与证据存在、在预算内）后作为 fact 应用，校验失败的操作丢弃并记录。复用 `exp/8.4-model-summaries` 的 effect 管道，但不放在压缩路径上。
  - 整理调用计入 `weighted_cost`，每个 run 的调用次数有上限。
  - 验收：long-horizon 与 9.4 的多会话场景上正确率不降、`weighted_cost` 不升；整理后诊断数下降。
  - 文件：`context/graph.rs`、`runtime/kernel/wire/effect.rs`；四端 SDK 的 effect 执行
- [ ] 任务 9.4：跨会话（索引与提升）
  - run 结束时，`rule` 与 `index` 笔记作为 memory candidate 提交（已有的 candidate ABI），附带 `derived_from` 所指证据的 digest。
  - 新会话只注入索引：有行数与字节上限的 `index` 笔记，正文用 `memory` 工具按 record_id 读取。
  - 跨会话整理在宿主侧离线运行，使用同一个 `consolidate_notes` effect，输入扩大为记忆库中的笔记和 transcripts 的检索结果。
  - 新基准 `multi-session`：同一领域连续 N 个会话，隐性规则只能从前面会话的失败中学到；指标为第 k 个会话的正确率与成本；变体为无记忆、只有笔记、笔记加整理，指令文本为另一变量。
  - 验收：第 N 个会话的正确率高于无记忆变体（配对），注入的 token 不随会话数线性增长。
  - 文件：`mm/memory_candidate.rs`、`mm/memory.rs`、`benchmark/context-lifecycle/multi-session.mjs`

放弃的方向：按缓存账决定压缩什么（8.3，`exp/8.3c`）；在压缩路径上同步生成摘要（8.4）；`RuleSummarizer` 作为记忆的载体。保留第 8 章的测量：缓存账本、按价格加权的配对 gate、long-horizon 基准。

## 交接（2026-11-02）

分支 `feat/context-lifecycle-stage0`。以下各项都已提交（`fda3aeb6` 及之后）：

- `orchestration/workflow/mod.rs`：新增 `DependencyContext::readable`，只有被 brief 截掉部分的依赖输出才随 spawn 下发（`dependency_outputs`），完整显示的输出不再提供 `read_result`。测试 `only_a_clipped_output_is_readable`；golden `golden_lifecycle_workflow_root.json` 已重新 bless。
- `context/renderer.rs`：turn 1 的 `→ focus:` 页脚只重复目标的第一段（最多 240 字符）。之前页脚复制整个 goal，而 goal 后面拼了 dependency brief，导致 brief 每次调用发两遍（24k → 49k）。测试 `the_focus_footer_repeats_only_the_goal_head`。
- `benchmark/context-lifecycle/live.mjs`：新文件，见任务 4.3 第二步；provider 错误会写进结果的 `error`，不再表现为空答案。
- 已验证：四个 SDK、contracts、parity、conformance 全部通过；CHANGELOG / MIGRATION 已记录。

已知问题：

- ~~DeepSeek replay~~（已修）：不是模型没给 reasoning，也不是流式累积丢了。kernel 在渲染时把移出保护窗口的旧 assistant 叙述（≥40 字符且带 tool call）替换成 `NARRATION_STUB`，而 Node / Python 的 reasoning replay 按 `content + tool_calls` 存取，内容一变就查不到，`requireReasoningReplay` 随即抛错。handle 模式工具调用多，旧叙述轮次更容易移出窗口，所以更常暴露。修复：OpenAI-chat 的 reasoning replay 对带 tool call 的轮次只按 tool calls 作 key（Node `reasoningReplayKey`、Python `reasoning_replay_key`）；Anthropic 的 native blocks 仍按内容作 key，未改。WASM 和 Rust 的 OpenAI provider 不回放 `reasoning_content`，不受影响。测试：Node `provider-narration-replay.test.ts`，Python `test_deepseek_replays_reasoning_after_the_kernel_rewrites_the_turn_text`。
- `benchmark-0.2.74` 的版本守卫（0.2.74 vs 0.2.77）早已失效，未处理。

下一步（2026-11-11 更新）：0.2.82 发布阶段 8 的测量（8.0–8.2），8.3、8.4 未通过 gate、不随版本发布。0.2.83 起做阶段 9（模型构建、修复、索引上下文图），先做 9.0。暂缓项：模型版 `RelevanceJudge`（作为入边权重的另一来源）、Anthropic 上报 compaction fact，等 long-horizon 基准能衡量时再排期。

换设备时的环境准备：

- 基线 worktree 在本机临时目录，新设备上重建：`git worktree add ../deepstrike-base 5b5308d3`，然后在其中构建 `node/dist`（`crates/deepstrike-node` 下 `npx napi build --platform --release`，`node` 下 `npm run build`）。
- 本分支同样要先重建 napi addon 和 `node/dist`，否则 Node 测试和基准跑的是旧 kernel。
- `.env` 不在仓库里，需要 `DEEPSEEK_API_KEY`、`DEEPSEEK_MODEL`（本次用 `deepseek-flash`）。
- WASM 测试要求 rustup 工具链在 PATH 上；Python 测试用独立 venv（仓库自带的 venv 不可用）。
- golden 变化时用 `BLESS_KERNEL_RECORD_FIXTURES=1 cargo test -p deepstrike-core --lib <测试名>` 重新生成。

## 版本规划（2026-11-02）

不跳到 0.3.0。每个版本一个分支、一个 PR，合入后发布；每个有破坏性变化的版本单独写 `MIGRATION-<上一版>-to-<本版>.md`。

| 版本 | 内容 | 状态 |
|------|------|------|
| 0.2.78 | 已发布（PR #4）；原分支 `feat/context-lifecycle-stage0`：阶段 0、1.2、2.1、2.2、3.1、3.2、4.1、6.1；1.1a 与 1.1b 的 item 身份部分；replay 修复；默认 edge mode 改为 `handle`；Rust workflow 编排 | 已发布 |
| 0.2.79 | 1.1 收尾：`ContextPartitions` 改为 item 表上的渲染视图，压缩管线按 item 操作；`until(event)` lease；scope / fidelity / authority 产生行为（scope 决定 renewal 与子 run 继承，fidelity 决定渲染形态，authority 决定谁能改）；handle 并入 item 表；history item 逐条明细（有上限） | 已发布（PR #5） |
| 0.2.80 | 较大的版本，含原 0.2.81：4.3 lab 工具；1.1c item 稳定 id 进入消息 ABI 与 5.1 吸收 provider 改写；4.2 引用证据分级；6.2 blackboard；6.3 Python、WASM、Rust 宿主程序 API。`MIGRATION-0.2.79-to-0.2.80.md` | 已发布（PR #6；暂缓：模型版 `RelevanceJudge`、Anthropic 上报 compaction fact） |
| 0.2.81 | 阶段 7：item 图。7.0 `knowledge-pressure` 基准与 0.2.80 基线；7.1 全部 item 有 id、边表只上报；7.2 交易单元来自 `answers` 边；7.3 沿边传播的保留与淘汰；7.4 跨 run 的边（只上报）。`MIGRATION-0.2.80-to-0.2.81.md` | 已发布（2026-11-03；7.2 决定不做） |
| 0.2.82 | 阶段 8：测量先行，缓存进入决策。8.0 真实缓存损失与 `prefix_breaks` 归因；8.1 按价格加权的 gate 与配对统计；8.2 `long-horizon` 基准与 0.2.81 基线；8.3 缓存感知的压缩；8.4 模型摘要作为 effect（取决于 8.2 的数据）。`MIGRATION-0.2.81-to-0.2.82.md` | 8.0–8.2 完成；8.3 未通过 gate、未合入，方向放弃（`exp/8.3c`）；8.4 实测未通过 gate，已撤回（`exp/8.4-model-summaries`） |
| 0.2.83 | 阶段 9：模型构建、修复、索引上下文图。9.0 清除测量混杂（工具调用标记泄漏、新基线、gate 默认配对）；9.1 会话内笔记；9.2 压缩读图。9.3 边界整理、9.4 跨会话视数据进入 0.2.84 | 计划中 |

0.2.82 内部顺序：8.0 → 8.1 → 8.2 → 8.3 → 8.4。8.0–8.2 只改测量，不改渲染，先在 0.2.81 上跑出新基线；8.3 是本版本唯一默认改变渲染的项，必须先过新的 gate；8.4 只有在 8.2 的数据满足前置条件时才做。

0.2.81 内部顺序：7.0 → 7.1 → 7.2 → 7.3，7.4 可与 7.2 / 7.3 穿插。7.1 与 7.2 不改变渲染结果，只有 7.3 改变决策，必须先过 lab gate。模型版 `RelevanceJudge` 若做，放在 7.3 之后，作为入边权重的另一来源，用同一个场景对比。

0.2.80 内部顺序（lab 先于被衡量的改动，否则分不清指标变化来自哪一项）：

1. 4.3 lab 工具（`crates/deepstrike-lab`），先在 0.2.79 的代码上跑出基线：regret、waste、cache_loss、成功率、成本。
2. 1.1c + 5.1：item 稳定 id 进入消息 ABI，provider replay 改按 id 作 key（替代按内容作 key，Anthropic native blocks 一并改）；provider compaction 与 `clear_tool_uses` 作为 fact 吸收（两者共用一次 ABI 扩展）。用 lab 验证 kernel 估算与实测 token 偏差 <10%。
3. 4.2 引用证据分级与 `RelevanceJudge`，用 lab 与纯词法版本对比：regret 和 waste 不升高，成功率不下降。
4. 6.2 blackboard、6.3 Python / WASM 宿主程序 API：与前三项无依赖，可穿插进行。

PR 较大，按上面的步骤分阶段 PR 合入同一个 release 分支，最后统一升版本号。

0.2.78 剩余步骤：

1. ~~重跑三个变体，每组 24 题~~（已完成，结果见 4.3 第二步重跑）。~~默认 edge mode~~：已改为 `handle`，见 MIGRATION。
   附带修复：DeepSeek 不给 reasoning 的 tool call 轮次改为回放空的 `reasoning_content`。
2. ~~任务 6.3 的 Rust 部分~~（已完成，`34850587`）：workflow 节点作为子 run 在父 runner 的 provider 和 plane 上运行，带依赖输出；模型提交的 `start_workflow` / `submit_workflow_nodes` 参数降为 canonical DAG。未做：信号驱动的抢占监控（Node 的 `monitorWorkflowPreemption`）、节点完成写入会话日志以支持恢复、Rust 侧 `start_workflow` 工具 schema 的导出。
3. ~~同步删除清单，升版本号到 0.2.78，开 PR~~（已完成，PR #4）。
4. ~~1.1a 与 1.1b 的 item 身份部分并入 0.2.78~~（已完成）。PR #4 的 docs 检查因缺英文版计划和 `memory-candidate-abi` 失败，已补 `docs/en/` 两份。

零散未做项（2.1 plan 变更时召回、3.2 Full 的 token 代价、3.1 的 `fields` 切片、`benchmark-0.2.74` 版本守卫）不单独排期，在碰到相关代码的版本里顺带完成。

## 删除清单

| 删除项 | 所在阶段 |
|--------|----------|
| ~~`context/execution.rs`、`evolution.rs`、四个 SDK 的 `shared_evidence.*` 和 `evolution.*`~~（已删） | 0.1 |
| ~~`context_prepared` 事件、`prepare_context_dispatch`、conformance 中的 digest 比对、`EvaluationContextBinding`~~（已删） | 0.1 |
| ~~`verifiable.rs`、chain_validator、CLI~~（已删，C1/C3 由 restore 自检覆盖） | 0.2 |
| ~~`KnowledgeEntry`~~（已删）；`Partition` 等按位置存储的分区类型（0.2.79 被 item 表取代） | 1 |
| ~~四个 `prefetchMemory`、三个 `recall-projection.*` 的投影逻辑~~（已删；文件只剩 `memoryFetchRecall` 点查） | 2 |
| ~~三个 `workflow-control-flow.*`（含 `summarizeDependency`）、边的 `summary` 模式~~（已删；`reference` 模式保留） | 2、3 |
| ~~`NodeKind::Loop/Classify/Tournament`、`orchestration/tournament.rs`~~（已删，连同 Reduce） | 6 |
| ~~“knowledge 追加便宜”的注释和文档~~（已改：`partitions.rs`、`docs/concepts/prompt-cache-design.md`） | 1 |

## 依赖与顺序

```text
阶段0 ──► 阶段1 ──► 阶段2 ──► 阶段3 ──► 阶段6
             ├──► 阶段5
             └──► 阶段4.2/4.3 ──► 阶段7（item 图）──► 阶段8（8.0/8.1/8.2 ──► 8.3 ──► 8.4）
                                                    └──► 阶段9（9.0 ──► 9.1 ──► 9.2 ──► 9.3 ──► 9.4）
阶段4.1 与阶段1 并行，最先产出基线
```

阶段 0 和阶段 6.1 都是纯删除，可以各自单独发版；其余阶段按依赖推进。

## 范围外

- ~~记忆写入侧（candidate ABI、`MemoryWriter`）冻结，不再新增写入机制。~~ 阶段 9 解冻：笔记的跨会话提升沿用 candidate ABI，不另建写入通道。
- ~~压缩管线内部的 `RuleSummarizer` 不在本计划内换成模型摘要；等 4.1 的数据说明它是瓶颈再议。~~ 已移入任务 8.4：现有基准到不了需要摘要的规模，数据由 8.2 的 long-horizon 提供。8.4 实测未通过 gate，已撤回；阶段 9 中 `RuleSummarizer` 降级为归档定位标签，记忆由模型写的笔记承担。
- 阶段 8 不新增 relation、lease 或 origin 种类。

## 风险

| 风险 | 缓解 |
|------|------|
| 删除证据层后，排查事故少了 `context_prepared` 这份记录 | journal + checkpoint 足以复原每轮的上下文决策；需要时用 `inspect`/`replay` 重建渲染结果 |
| 模型相关性判断增加成本和延迟 | 批量打分、小模型、仅在压力超过阈值时触发、词法兜底 |
| durable item 延迟到 boundary 才提升，可能来得太晚 | `urgent` 开关并记录代价；用 regret 观察 |
| handle 模式下子 agent 不主动 page-in | 提升 preview 质量、在 brief 中显式提示；用 regret 和成功率评估默认 edge mode |
| 删除 NodeKind 会让现有声明式 WorkflowSpec 无法运行 | 接受这个破坏；迁移文档提供改写示例，加载旧 spec 时报错并指向文档 |
| item 图让 checkpoint 和报告变大 | 边数量设上限（两端在场 + 最近退场的一批），报告里的边有上限 |
| 每条 history 消息都要对全部 knowledge 判断引用，建边增加开销 | 沿用 4.2 的特征词集合，按 item 缓存词集；lab 记录每轮 kernel 耗时 |
| 沿边传播放大了一次误判的引用 | 只传一跳、强引用权重高于词法；7.3 必须过 lab gate |
| 不同 provider 的缓存粒度和规则不同，实测缓存损失有噪声 | 8.0 的容差设为 15%；归因只用于比较同一 provider 下的变体 |
| 推迟压缩让上下文更长，可能影响注意力质量和正确率 | 窗口硬上限优先；8.3 的 gate 同时看正确率与 `weighted_cost` |
| long-horizon 基准每轮成本高，样本量不够 | 脚本化工具环境、只有答题需要真实模型；每个变体 30 题，配对比较 |
| 摘要 effect 增加一次模型调用和延迟 | 只在 auto 压缩时触发；摘要调用计入 `weighted_cost`；失败退回 `RuleSummarizer` |
| 模型不写笔记，或写得没用 | 方向性指令与指令变体实验；换出预告给出写笔记的时机；9.3 的整理修复 |
| 错误的笔记被固化，之后反复误导 | 每条笔记必须引用证据；整理以证据为准；`supersedes` 留下修正记录 |
| 笔记占满窗口 | `notes_budget_tokens` 上限；超出时由模型合并或退役，kernel 不代删 |
| 整理 effect 增加成本和延迟 | 只在有诊断时、只在 boundary 触发；每个 run 有调用上限；计入 `weighted_cost` |

## 成功指标

- 长任务基准：成功率不降的前提下，平均输入 token −25%，cache_loss −50%，regret 和 waste 有基线且逐版本下降
- 四个 runner 合计行数 −30%；core 删除证据层后净减约 7k 行（`chain_validator` 3.8k、`evolution.rs` 1.3k、`execution.rs` 1.2k、`verifiable.rs` 0.7k，扣除保留的 C1/C3）
- 改一条生命周期策略只需改 kernel 一处（作为 PR 检查项）
- fan-out→synthesize：下游初始 token 与 fan-out 宽度无关
- 开启 server compaction 时，kernel 估算与实测 token 偏差 <10%
- 阶段 8（0.2.82）：kernel 自报的 cache_loss 与实测偏差 ≤ 15%；ledger 与 long-horizon 的 `weighted_cost` 低于 0.2.81 基线，正确率不降（配对 95% 置信区间）
- 阶段 9（0.2.83 起）：long-horizon 正确率高于 `0.2.83-baseline`，`weighted_cost` 不升（配对 95% 置信区间）；multi-session 第 N 个会话的正确率高于无记忆变体，注入 token 不随会话数线性增长；每条笔记都能沿 `derived_from` 指到证据
- item 图（0.2.81）：`knowledge-pressure` 场景的 regret 与 waste 低于 0.2.80 基线，成功率不降；每个 regret 和 waste 都能在报告里指到具体的边
