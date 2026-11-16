# 内核提炼：让四个决定只有一处实现

> 起草 2026-11-15，基于 `main`（`df26eabf`，v0.2.83）。执行者：Opus。
> 前序：[上下文优先计划](./context-lifecycle-first.md)（阶段 0–9）、[DEC-13](../decisions/013-model-led-context.md)。
> 本文只覆盖 `crates/deepstrike-core`。四端 SDK 与 `runtime/kernel/wire` 的 ABI 层是下一轮。

## 1. 目标

框架的核心原则已经写在上下文优先计划里：

> 在有限的注意力预算下，为每一条信息决定**作用域、形态、寿命和写入权**。

阶段 0–9 把这句话落成了 `ContextItem` 和一张 origin 默认值表（`context/item.rs` 的 `ItemOrigin::defaults`）。这一轮不加机制，只做提炼：

1. **表说的就是代码做的。** 每条模型可见的信息都是 item 表里的一行，四个决定各只有一处实现。
2. **内核只做机制，不做语义。** 0.2.82 之前留下的规则机制按 DEC-13 逐项实测，没用的删除。
3. **内核对模型说的每一句话都能数、能关、能换。**

完成的标志：

- 四个维度各只有一个类型和一个执行点（见 §4）。
- origin 表里的每个值都有生产者和消费者，没有只在表里出现的值。
- `ContextConfig` / `ContextPolicy` 里没有无人读取的字段，没有默认关闭的开关。
- §3.5 的每项机制都有"实测后保留"或"已删除"的结论，记录在本文。
- `context/` 非测试代码行数下降（现为约 12.4k 行，含文件内测试）；`ContextManager` 的字段数下降（现为约 45 个）。

## 2. 审计范围与可信度

已通读：`context/` 全部模块的结构，`item.rs`、`partitions.rs`、`compression.rs`、`renderer.rs`、`utility.rs`、`summarizer.rs`、`task_state.rs`、`renewal.rs`、`config.rs`、`policy.rs` 的实现；`mm/`、`syscall/`、`signals/`、`governance/repeat_fuse.rs`、`scheduler/entropy.rs`、`scheduler/state_machine/eviction.rs` 的实现或头部；`wire/config.rs`、`wire/effect.rs`、`wire/syscall.rs` 的类型清单。

只看了轮廓：`scheduler/state_machine/mod.rs`（2.3k 行）、`wire/driver.rs`（3.5k 行）、`wire/checkpoint.rs`、`orchestration/`。涉及这些文件的结论标了"需核实"，动手前先读代码确认。

下文"无使用"的结论来自对 `crates/deepstrike-core/src` 的 grep，没有查四端 SDK 和 `contracts/`。删除任何带 serde 的类型或变体前，先查 SDK、`contracts/`、`tests/fixtures/`。

## 3. 现状：四个决定各有几处实现

### 3.1 表覆盖 13 种 origin，真正按 item 存的只有两类

| 信息 | 实际存储 | 寿命由谁管 |
|---|---|---|
| knowledge（skill、recall、reference、blackboard） | `KnowledgePartition.entries: Vec<ContextItem>` | `ContextItem.lease`，表驱动 |
| history（user、assistant、tool_result、provider_summary） | `Partition` 的行，带 `ItemTag` | 压缩管线与 renewal |
| signal、内核提示 | `ContextPartitions.signals: Vec<String>` | `LoopStateMachine.delivered_signals_len`：请求提交后按前缀长度清掉 |
| task state（goal、plan、directives、progress、scratchpad、blocked_on、recent_actions、compression_log） | `TaskState` 的 14 个字段 | 各自的上限常量（8 / 6 / 64，渲染 3 条）；renewal 清 scratchpad |
| note | `TaskState.notes`，有 item id，但不是 `ContextItem` | 模型退役；预算拒写 |
| 依赖输出 | `ContextManager.dependency_bodies: BTreeMap` | 无 |
| provider replay | `ContextManager.item_replays` | 随 item 清理 |
| skill 激活 | `ContextManager.active_skills: BTreeMap<_, Option<u32>>` | 到期 turn，另有 `Lease::Until(SkillDeactivated)` 管正文 |

表里因此有一批值没有生产者或消费者（core 内 grep，排除 `item.rs` 自身）：

- `ItemOrigin::Signal`、`ItemOrigin::TaskState`、`ItemOrigin::Note`：没有任何地方构造这三种 origin 的 item。
- `Lease::SingleUse`：无使用。表里说 signal 是 `SingleUse`（"被引用后离开"），实际语义是"随请求提交离开"，两者不一致。
- `Fidelity::Structured`：无使用。
- `ItemScope::Agent`：没有任何 origin 取这个值。

### 3.2 形态：三套词汇

- `Fidelity`（`item.rs`）：full / structured / excerpt / handle。
- `Residency`（`mm/handle.rs`）：Resident / Collapsed / External / PagedOut。`ContextManager::history_fidelity`（`manager.rs:503`）在读取时把它翻译成 `Fidelity`；`ItemTag.fidelity` 自己只记 excerpt。
- `ObjectDescriptor` / `ObjectKind`（`mm/handle.rs:274`）：文档自述为"与 Handle 并行的增量"，用于跨任务读取（约 37 处引用）。

另有两处形态变化不在任何一套词汇里：渲染时的叙述折叠（`renderer.rs` 的 `project_assistant_narration`）和 collapse 预览，都是渲染函数里的临时投影，item 上不留记录。

`ObjectKind` 的 9 个变体在 core 里只有 `Artifact` 有一处构造（审计初稿误写为 `HandleKind::WorkingPage`/`AgentResult`；`HandleKind` 的 4 个变体都经 wire 字符串映射使用）。去留并入 1.4 的 `ObjectDescriptor` 决定。

压缩动作也有三套词汇：`PressureAction`、`EvictionOp`、`KernelPressureAction`。`EvictionOp` 携带的参数在执行处被丢弃（`eviction.rs` 的 `Snip { per_msg_ratio: _ }`、`AutoCompact { preserve_turns: _ }`），计划等于一次改名。`CollapseCompactor` 与 `AutoCompactor` 的函数体逐行相同（`compression.rs:425–519`），区别只在调用方传入的 target。

### 3.3 作用域：只在两处生效，其中一处是空操作

`ItemScope` 的消费点只有两个：

- `renewal.rs:59` 的 `survives_renewal`：只有 `Edge` 返回 false，而 `Edge` 作用域的依赖输出存在 `dependency_bodies`，不在 knowledge 里。这个过滤器现在删不掉任何东西（需核实）。
- `manager.rs:1681` 的 `inherited_by`：决定子 run 继承哪些 knowledge，真实生效。

`MemoryScope`（tenant / namespace）、`ContextInheritance`、workflow 的 quarantine 是 effect 准入一侧的隔离边界，不属于这一轮要合并的对象，只需在文档里写清它们和 `ItemScope` 的关系。

### 3.4 写入权：三种方言

- `Authority`（host / kernel / model / provider）：只在 `push_knowledge_item` 一条路径上检查（`manager.rs:1683–1737`）。
- `InputAuthority` + `SyscallCausation`（`wire/envelope.rs`、`wire/syscall.rs`）：ABI 层判断"这个输入是谁发的"。
- `MemoryAuthor` + `MemoryTrustLevel` + attestation（`mm/memory.rs`、`mm/memory_candidate.rs`）：长期记忆的写入权，最完整。

task state 的写入权是隐式的：`TaskUpdate` 有 7 个字段，`update_plan` 的 schema 只暴露其中 4 个（plan、current_step、progress、blocked_on），其余由宿主命令写。`SyscallRequest::UpdateTask` 的注释说"与 `HostCommand::UpdateTask` 权限不同"，但没有一张表说明哪个字段谁能写。

### 3.5 内核仍在替模型判断的地方

DEC-13 的"后果"一节写明：0.2.82 之前的规则机制没有按新标准测过。逐项列出，并注明它判断了什么：

| # | 机制 | 位置 | 内核替模型判断的内容 |
|---|---|---|---|
| M1 | 叙述折叠，默认开 | `renderer.rs` `project_assistant_narration` | 旧的 assistant 文本没有价值。8.2 实测 long-horizon 每题因此损失约 78k 缓存 token |
| M2 | 效用打分归档 | `utility.rs:137` | 与 goal 词法重叠 ×4000、含"未解决"标记 +20000、含"错误/决定"标记 +6000 |
| M3 | `RuleSummarizer` 六槽 | `summarizer.rs` `classify_text` | 按关键词把归档内容分进 constraints / decisions / … ，最近 3 条渲染进状态块 |
| M4 | 五级压力阶梯 + 空闲衰减 | `config.rs`、`pressure.rs`、`mm/handle.rs` `plan_eviction` | 0.70 / 0.80 / 0.90 / 0.95 / 0.98 五个阈值各触发一种改写；空闲 60 分钟触发 micro |
| M5 | 压力过 collapse 阈值后每轮折叠旧工具结果 | `manager.rs` `recompute_handle_residency` | 8.0 发现这是每轮断点来源之一 |
| M6 | knowledge 预算淘汰 | `mm/value.rs`、`partitions.rs:447` | 按 kind 权重、词法引用计数、recall 租期 8 轮打分 |
| M7 | 状态块页脚 | `renderer.rs:185` `salience_footer` | `did:` / `next: advance the goal` / `must:` / `STOP:` |
| M8 | criteria gate | `state_machine/mod.rs:1274` | 模型要结束时注入一次自检 |
| M9 | task state 里与笔记重叠的字段 | `task_state.rs`、`update_plan` schema | `progress`、`blocked_on`、`scratchpad`：笔记默认开启后，模型有两处记同一件事 |

两点背景，决定了这些要重测而不是直接删：

- **笔记改变了基线。** 8.3 的结论（推迟轻量层会让模型丢掉累计计数）是在没有笔记的条件下得到的。笔记默认开启后，压缩层的每一条旧结论都要在新基线上重测。
- **M1 的混杂因素已排除。** 8.3 里"叙述保留到边界"与循环相关，是因为泄漏的 DeepSeek 标记留在上下文里；9.0 已在 provider 层修掉。

`RepeatFuse`（deny / terminate）是 effect 准入一侧的熔断，不在此列，保留。

### 3.6 配置面与陈旧说明

- `ContextConfig.autocompact_buffer` 与 `autocompact_threshold()`：只在 `config.rs` 内出现，无人读取。
- `ContextConfig.verbose_control_notes`：默认关闭的开关，3 个文件读取。
- `preserve_recent_units`（渲染用）与 `preserve_recent_turns`（压缩用）：两个含义相近的旋钮。
- 同名的 `ContextPolicy` 有两个：`context/policy.rs` 与 `wire/config.rs`，字段不同（后者多 `knowledge_budget_ppm`、`prompt_budget`、`delegated_tiers`）。
- `ContextConfig::default()` 的 `notes_budget_tokens` 是 0，wire 默认是 600。直接构造 `ContextManager` 的测试拿到的是关闭的笔记。
- `EntropyWatchConfig.notify_model`：默认关闭的开关。
- 陈旧注释：`mm/handle.rs:233` 与 `manager.rs` 提到已不存在的 `effective_rho`；`mm/handle.rs:415–418` 说"SDK 在带外用 LLM 升级摘要"，与 DEC-13 第 1 条冲突，需核实四端 SDK 是否还有这条路径；`MemoryTierHint::Semantic` 是它的遗留。
- 陈旧文档：`docs/architecture/execution-model.md` 的 Phase 4 写"Summarize 由 SDK 侧 summarizer 执行"。

### 3.7 内核对模型说的话散在 11 个文件

折叠占位、页脚、criteria 自检、输出截断提示、回滚说明、治理提示、预算告警、笔记标题、`[context resumed]`、元工具说明，分布在 `manager.rs`、`renderer.rs`、`state_machine/mod.rs`、`task_state.rs`、`compression.rs`、`signal.rs`、`rollback.rs` 等 11 个文件。计划里说过"指令文本是实验变量"，现在没法把它们作为一个整体计数或替换。

## 4. 目标形态

**一张表，四列，各一个执行点。**

| 决定 | 唯一类型 | 唯一执行点 |
|---|---|---|
| 作用域 | `ItemScope` | renewal 与 spawn 继承都调用同一个 `visible_to(run)` 判断 |
| 形态 | `Fidelity`（吸收 `Residency`） | 渲染器只读 item 的 fidelity；所有降级都写回 item |
| 寿命 | `Lease` | 一个 `sweep(turn, event)`：signal、skill、recall、note 都走它 |
| 写入权 | `Authority` | 一个 `admit_write(origin, writer)`：宿主命令、模型工具、内核自己都过它 |

**所有模型可见的信息都是 item。** signal、内核提示、note、依赖输出进入同一个存储；`ContextPartitions` 的四个槽只是按 origin 与状态派生的渲染位置（上下文优先计划的 D3）。task state 保留为一个 `Fidelity::Structured` 的 item，字段按 §5 阶段 2 的结果精简。

**压缩是"按表降级形态"，不是五种算法。** 目标形态（阶段 2 实测后才能定）只有两个动作：把旧工具结果降为 handle（可用 `read_result` 读回，不丢信息），以及在边界处把最旧的单元归档。选谁、何时由 item 的年龄和预算决定，不读内容。

**内核的话有一个出处。** 所有内核写给模型的文本集中在一个模块，作为 `origin = system` 的 item 进入上下文，lifecycle report 报告每次请求里内核文本占多少 token。

## 5. 任务

三类任务，规则不同：

- **R（重构）**：不改变任何一次渲染的字节。验收是现有 golden 不变，或只有 checkpoint / report 字段变化。
- **D（直接删除）**：无人使用的代码。验收是编译通过、测试通过、四端 SDK 与 contracts 无引用。
- **G（过 gate）**：会改变模型看到的内容。按 DEC-13 实测后决定，本文记录结果。

### 阶段 0：清理（D，一个 PR）

- [x] 0.1 删除 `autocompact_buffer`、`autocompact_threshold()`。
- [x] 0.2 ~~删除 `HandleKind::WorkingPage`、`HandleKind::AgentResult`~~。核实时发现这两个变体不存在，`HandleKind` 四个变体都在用；本项取消，`ObjectKind` 的去留并入 1.4。
- [x] 0.3 `CollapseCompactor` 与 `AutoCompactor` 合并为一个按 target 归档的实现；管线里两级保留各自的 `PressureAction` 标签，渲染与观测不变。
- [x] 0.4 `EvictionOp` 去掉执行时被忽略的参数，或让执行处真的使用它们，二选一，以代码更少者为准。`plan_eviction` 的文档里关于"未来的 cache-cost 排序"的段落删除（8.3 已放弃这个方向）。
- [x] 0.5 修正陈旧注释与文档：`effective_rho`、"SDK 带外 LLM 摘要"、`execution-model.md` Phase 4。核实四端 SDK 是否仍有带外摘要路径；有则报告，不在本阶段处理。
- [x] 0.6 `verbose_control_notes`：查四端 SDK 是否暴露。未暴露则删除开关与 verbose 分支。
- [x] 0.7 `ContextConfig::default().notes_budget_tokens` 改为 `DEFAULT_NOTES_BUDGET_TOKENS`，受影响的单测显式传 0。
- 验收：`cargo test --workspace` 通过；kernel golden 逐字节不变（0.7 若使 golden 变化，说明有测试依赖了错误的默认值，逐个修正并在 PR 里列出）。
- **执行记录（2026-11-15，分支 `refactor/kernel-distill-phase0`）：**
  - 0.3 两个 compactor 合并为 `ArchiveCompactor`，管线两级仍各自标 `ContextCollapse` / `AutoCompact`。
  - 0.4 选了"去掉参数"：`EvictionOp::Snip`、`EvictionOp::AutoCompact` 变为无字段；`plan_eviction` 去掉 `preserve_turns` 参数，`ContextManager::plan_compaction_params` 改为 `compaction_target_tokens`。这两个函数从 crate 根导出，属于 Rust 公开 API 的破坏式变更，发布时写入 CHANGELOG。
  - 0.5 核实结果：**Node SDK 仍有带外摘要路径**（`RunnerOptions.asyncSummarizer` → `summary_upgraded` 事件，可选、默认关闭）。用户裁决删除：去掉 `asyncSummarizer` 选项、`AsyncSummarizer` 类型、后台改写与 page-in、对应测试与文档。随后用户要求完整移除：`summary_upgraded` 事件从 contract、四端注册表、Node / WASM / Python 类型与回放逻辑中删除。旧日志仍可加载（Node / WASM 解码不拒绝未知 kind），其中的 `summary_upgraded` 被忽略，回放改用 `compressed` 事件自带的规则摘要。
  - 0.6 同时删除了只服务于 verbose 分支的 `rollback_reason_message`。
  - 0.7 受影响的测试 4 个：`meta_tool_schemas_are_sorted` 的期望值加入 `note`；`top_level_run_without_spec_exposes_no_task_tools`、`exposure_baseline_narrows_even_without_a_ceiling`、`tests/rust` 的 `agent_run_spec_capability_filter_enforcement` 显式设 `notes_budget_tokens = 0`（它们测的是任务工具暴露面）。
  - 验收：`cargo test --workspace` 全部通过；没有任何 golden fixture 变化。Node（1421）与 WASM（297）测试全部通过，含 SDK conformance；本机 Python venv 解释器失效未能跑 pytest，用 Python 3.12 导入 `session_log` 验证了 union 与注册表一致（51 种事件）。

### 阶段 1：表即实现（R，每个任务一个 PR）

顺序按依赖排列。每个任务完成后 origin 表里少一个"只在表里出现的值"。

- [x] 1.1 **signal 与内核提示成为 item。**（2026-11-16）
  - `partitions.signals: Vec<String>` 改为 item 行（origin `Signal` 或 `System`）。
  - 新增一种 lease 表达"随携带它的请求提交而离开"，替换 `delivered_signals_len` 的前缀计数协议；`Lease::SingleUse` 若不再有别的用途就改名为它，而不是并存。
  - lifecycle report 的 `history` 汇总里出现 signal 的条数与 token。
  - 验收：状态块渲染逐字节不变；signal 相关的状态机测试全部通过；checkpoint 能恢复旧格式的 `signals`。
  - **执行记录。** 动手前先验证了前缀计数协议有没有实际缺陷：有。`delivered_signals_len` 不进 checkpoint，在 provider 请求未结清时取的 checkpoint 恢复后，内核忘了这个请求已带过哪些 signal，下一次请求再发一遍；原内核不会。原内核与恢复后的内核因此分叉，违反回放确定性。
    - 每条 signal 是 `SignalItem { text, delivered }`（`partitions.rs`），发出请求时整体标记、请求结清时移除已标记的、请求没发出去时取消标记；`delivered_signals_len` 删除。已标记的总是前缀，所以 checkpoint 只存个数 `signals_delivered`（缺省 0，旧 checkpoint 照旧恢复）。
    - `Lease::SingleUse`（没有 item 用它，表里却说 signal 是它）改名为 `Lease::UntilDelivered`，表说的就是代码做的；知识侧的 sweep 不按它过期。删掉了只测那条无人路径的单测。
    - 回归测试 `a_delivered_signal_is_not_sent_again_after_a_restore`，去掉改动即失败。golden 没有变化。
    - **与原计划的两处偏差。** ① signal 没有从共享分配器取 item id：取了的话之后每个历史 item 的 id 都后移一位，渲染里带 `item_id`，golden 会变，违反本任务"渲染逐字节不变"的验收；等到 1.7 内核文本集中时与内核提示一起决定。② lifecycle report 没有加 signal 的条数与 token：`history` 汇总的语义是"进入历史的 item"，signal 不进历史，硬塞会混淆；改由 1.7 的 `kernel_text_tokens` 统一计数。
    - `ContextPartitions.signals` 是公开字段，`tests/rust` 直接读它，这是 Rust 公开 API 的破坏式变更，已写入 CHANGELOG 与 MIGRATION。
- [ ] 1.2 **note 成为 item。** `TaskState.notes` 的每条笔记是 origin `Note` 的 item，预算拒写、标题重写、图上的三种边保持不变。`notes_tokens()` 改为对 item 求和。
  - 验收：`context/notes.rs` 的 8 个测试不改断言通过；`notes:` 块逐字节不变。
- [ ] 1.3 **依赖输出成为 item。** `dependency_bodies` 改为 origin `DependencyOutput`、scope `Edge`、fidelity `Handle` 的 item；`read_result("dep:<agent>")` 读 item。完成后 `survives_renewal` 才有真实的过滤对象，补一个测试证明 renewal 之后依赖输出的处理符合表的定义；如果现有行为是"renewal 后仍可读"，以现有行为为准并修正表。
- [ ] 1.4 **形态合一。** `Residency` 的四个状态并入 item 的 fidelity（handle 需要携带 locator 与 digest）。`HandleTable` 降为按 `call_id` 查 item 的索引，不再持有独立状态。`history_fidelity()` 删除。
  - 叙述折叠与 collapse 预览改为"降级写回 item 的 fidelity，渲染器只读"。这一步不改变何时折叠，只改变记录在哪。
  - `ObjectDescriptor`：先读 `scheduler/cross_operation.rs` 与 `wire/driver/syscall.rs` 的用法，再决定它是 item 的跨任务投影（保留为一个纯函数）还是独立概念（保留并在文档中说明边界）。不要为了合并而合并。
  - 验收：golden 渲染逐字节不变；缓存账本的断点归因不变。
- [ ] 1.5 **寿命合一。** `sweep_expired_leases`、`sweep_expired_skill_leases`、`sweep_expired_item_leases`（`state_machine/mod.rs` 的 `feed` 开头连续调用）合成一个按表执行的 sweep。`active_skills` 的到期 turn 并入 skill item 的 lease。`CapabilityLease` 属于 effect 准入，保持独立，在文档里说明。
- [ ] 1.6 **写入权合一。** 新增 `admit_write(origin, writer, op)`，表里为每种 origin 写明谁能创建、替换、删除。`push_knowledge_item` 现有的三段检查迁入；`TaskUpdate` 的 7 个字段各标明写入方，模型经 `update_plan` 写不在名单里的字段时拒绝。
  - 验收：一个表驱动测试枚举 origin × writer × op 的全部组合并固定结果。现有行为与表冲突的地方逐条列在 PR 里，由用户裁决，不自行改行为。
- [ ] 1.7 **内核文本集中。** 新建 `context/voice.rs`（名字可改），所有内核写给模型的字符串常量与格式函数迁入；lifecycle report 增加 `kernel_text_tokens`。
  - 验收：渲染逐字节不变；`grep` 在该模块之外找不到面向模型的字面量（测试除外）。
- [ ] 1.8 **`ContextManager` 按职责拆开。** 1.1–1.7 之后再做，届时字段已减少。按现有字段的自然分组拆为：item 存储、预算与压力（含估算校准）、缓存账本、能力与工具暴露（skills、capabilities、meta-tool schema）、报告。只移动，不改逻辑。
- [ ] 1.9 表里仍无生产者或消费者的值，删除。

阶段 1 的 checkpoint 形状会变。每个任务保留旧 checkpoint 的恢复路径并加测试，变化写进 `MIGRATION-0.2.83-to-<下一版>.md`。面向宿主的 ABI（`KernelInput`、`KernelEffect`、wire `ContextPolicy`）在阶段 0–1 不变。

### 阶段 2：DEC-13 审计（G）——已完成第一轮，M1–M7 已删除

**方法：先测整体剥离，再二分。** 逐项各跑 30 对成本太高。

1. 在实验分支上给 M1–M9 各加一个仅用于实验的开关（不合入主线）。
2. 定义变体 `minimal`：
   - M1 关闭叙述折叠；
   - M2 归档按"最旧优先"，强制保留只剩最近 N 个单元和未配对的工具调用；
   - M3 摘要改为一行机械记录（归档条数、token、涉及的工具名），不分槽；
   - M4 去掉空闲衰减；snip 与 micro 合并为"旧工具结果降为 handle"一个动作；
   - M5 折叠只在边界批量做；
   - M6 引用只计强证据，去掉 kind 权重；
   - M7 去掉页脚；
   - M8 关闭 criteria gate；
   - M9 `update_plan` 去掉 `progress`、`blocked_on`，删除 `scratchpad`。
3. 用 DEC-13 的默认 gate，方向是**现有默认配置为候选、`minimal` 为基线**：现有机制作为一个整体，必须在目标场景里让正确率或每答对一题的成本显著变好，否则整体删除。
   - 场景：long-horizon（压缩密集，M1–M5）、ledger（小窗口）、multi-session（M9 与笔记、记忆的交互）、knowledge-pressure（M6）；fan-out 作回归。
   - 先各跑 10 对看方向，再补到 30 对下结论。
4. 现有配置通过 gate 时，按组二分：压缩组（M1–M5）、提示组（M7、M8、M9）、knowledge 组（M6），直到定位到真正有贡献的机制。
5. 每个结论写回本节：数据、去留、删除的提交。保留的机制去掉实验开关；删除的机制连同配置字段、SDK 选项、contract、测试、文档一起删。

注意事项：

- 读结果前先排除协议层混杂（泄漏的工具标记、重复的 call id、跑到轮数上限的运行），做法见上下文优先计划 9.0。
- 实测花用户的钱。启动前从 `benchmark/context-lifecycle/results/` 的既有数据估算本次花费，查 DeepSeek 余额，把预计花费报给用户，得到同意再跑。
- `minimal` 在 long-horizon 上如果出现大量跑到轮数上限的运行，先停下来分析，不要跑满 30 题。

**执行记录（2026-11-16）。** 用户批准预算 30 元，并决定先做阶段 2、阶段 1 剩余任务等审计后再定。

- **做法。** 实验分支 `exp/stage2-audit`（不合入）上，环境变量 `DEEPSTRIKE_EXP_MINIMAL` 把 M1–M9 剥成 `minimal`（`all` 或 `m1,m5` 这样的列表）；不设时 1158 个内核测试全过。没有按计划等 1.4：实验开关直接挂在现有代码上即可。
- **实测**（`deepseek-flash`，同题配对，`results/stage2-*.jsonl`，变体 `s2-default` 为候选、`s2-minimal` 为基线；按 long-horizon 历史单价估算约 15 元）：

  | 场景 | 配对数 | 正确：default / minimal | 每答对一题的成本 | gate |
  |---|---|---|---|---|
  | long-horizon | 10 | 6 / 4 | default 便宜 22% | 样本不足 |
  | ledger | 30 | 28 / 28 | default 贵 62% | default 没有显著变好 |
  | knowledge-pressure | 30 | 25 / 27 | default 贵 76% | **default 显著变差** |
  | multi-session | 48 会话 | 34 / 32 | default 便宜 9% | default 没有显著变好；按序列 3 好 1 差 |
  | fan-out | 12 | 12 / 12 | default 便宜 16% | 回归，无差异 |

  - 成本差几乎全来自缓存：minimal 在每个场景的缓存命中率都更高，default 的前缀断点是 minimal 的 2–7 倍，断点原因几乎都是 `collapse`（M5）与 `narration`（M1）。
  - long-horizon 逐题读过答案：两边错题都是计数差 1–2 行，没有只出现在某一边的错误类型；10 对里不一致的 6 对是 4 比 2，在随机波动范围内。minimal 没有跑满轮数（0/10），唯一一次跑满轮数在 default。
- **决定（用户 2026-11-16）："大部分都删除。"** 删除 M1–M7；M8（criteria gate，只在宿主给出验收标准时触发，这一轮没有任何基准设置它，没有数据）与 M9（`update_plan` 的 progress / blocked_on 与 scratchpad，属于四端 SDK 的 API）暂留，另行决定。long-horizon 没有补到 30 对。
- **删除的做法与验证。** 先把实验开关按 M1–M7 剥离、M8–M9 保留固定下来，重新生成的 golden 与实验分支上测过的 `minimal` 逐字节相同；再删掉死代码与配置面，此时 golden 的差异只剩被删的配置字段与随之变化的摘要值。具体：
  - M1 叙述折叠：删除投影、`collapseOldAssistantNarration`、缓存断点原因 `narration`。
  - M2 效用打分：`context/utility.rs` 换成 `context/archive.rs`，最旧优先，只按规则保留最近的单元。
  - M3 规则摘要：删除 `summarizer.rs`；压缩记录是一行 `archived N messages (~T tokens); tools used: …`。
  - M4：删除空闲衰减（`idleMicroCompactMinutes`、两个时间戳）与单独的 snip 档；snip 是 micro 的第一阶段，阈值去掉 `snip`，`micro` 默认 0.70（即测过的 minimal 的行为）。
  - M5：删除每轮折叠旧工具结果（`recompute_handle_residency`）；`Collapsed` 形态仍由 provider 侧清理产生，保留到 1.4。
  - M6：knowledge 留存分只计强引用，去掉 kind 权重。
  - M7：删除状态块页脚与它背后的 `recent_actions`。
  - checkpoint 中 `recent_actions`、`last_activity_ms`、`last_compact_ms` 只读不写，旧 checkpoint 仍可恢复。
  - 测试：断言被删行为的测试删除；两个 Python 测试原先靠规则摘要撑高压力或依赖压缩时机，改为直接构造条件（长 goal、在 state turn 里查重新激活的 skill）。四端全量测试、复现脚本、文档与契约检查全部通过。
- **对阶段 1 的影响。** 1.4（形态合一）的范围缩小：叙述折叠与每轮折叠已不存在。1.7 要集中的内核文本少了页脚与规则摘要。1.2、1.3、1.5 仍然没有找到实际收益，建议在 1.6、1.7 之后再看。

阶段 2 结束后做一次配置收敛：`PressureThresholds` 只留仍被使用的阈值；`preserve_recent_units` 与 `preserve_recent_turns` 合并；两个同名 `ContextPolicy` 留一个权威，另一个改名或删除；`EntropyWatchConfig.notify_model` 按"没有休眠开关"删除（需用户确认，见 §8）。

### 阶段 3：文档

- [ ] 3.1 新增一页 `docs/concepts/context-item.md`：只放 §4 的表和 origin 默认值表，说明每一列在代码里的唯一执行点。其它文档引用它，不再各自解释分区。
- [ ] 3.2 更新 `docs/architecture/agent-os.md` 的 P3、`execution-model.md` 的 Phase 3–4、`docs/guides/context-engineering.md`。
- [ ] 3.3 `docs/en/` 同步。仓库的 docs 检查要求中英文成对（PR #4 曾因此失败）；本文的英文版也要在第一个 PR 前补上。

### 阶段 4：记忆的证据链与出错时的度量

> 2026-11-15 补入。起因是对照 Anthropic《The components of agent memory》（2026-11-06）复盘 0.2.78 以来的上下文机制。原文未读，只读了一份转述；其中的实验数字不作依据。
> 本阶段超出本文开头"只覆盖 `crates/deepstrike-core`"的范围：4.1 给一个 wire 类型加可选字段，4.2–4.4 改基准与 `deepstrike-lab`。

**复盘结论。** 文章把记忆分成四层（原始记录、笔记、知识表示、注入上下文），由一个离线整理过程在层间搬运。对照代码：

| 文章的层 | 现有实现 | 状态 |
|---|---|---|
| 原始记录 | 追加写的 session log；history item 与 handle，`read_result` 读回 | 已有。阶段 0 删掉 `summary_upgraded` 之后，没有任何事件会改写回放内容 |
| 笔记 | `note` 工具，默认开启；`derived_from` / `links_to` / `supersedes` 由内核记边 | 已有，过了 gate |
| 知识表示 | item graph，运行内的投影，不是存储 | 已有 |
| 注入上下文 | 首轮与每次 renewal 后 recall，注入索引行；`memory(record_id)` 点读 | 已有。只在边界处刷新，运行中的会话不会被后台改变知识基础 |
| 向上流动 | 模型调用 `propose_memory` | 已有，过了 gate |
| 离线整理 | 9.4 的整理 run | 实测后删除：multi-session 33/48 → 33/48，每答对一题的成本 +272% |

文章的"框架负责边界，模型负责判断"和"强制只记失败反而降低收益"与 DEC-13 是同一个结论，不改变阶段 0–3。它指出的缺口有两个是真的：

- **证据链在"笔记 → 记忆"这一跳断了。** 笔记的证据由内核解析成 item（`notes.rs` 的 `note_evidence`）并记边。`propose_memory` 的 `evidence_refs` 是模型填的字符串，`driver/syscall.rs` 原样放进 `CanonicalMemoryWrite`，不解析、不校验。`memory_written` 事件只有 `record_id`、`name`、`size_bytes`、`turn`。一条记忆出错时，只能靠 `session_id` 加 `turn` 去日志里找那次 `llm_completed`。
- **没有测过记忆出错的情形。** multi-session 的每个序列只有两条规则，规则从不变化，宿主接受每一条提案。9.4 的结论因此是"记忆干净时整理没有用"，不是"整理没有用"。记忆按 `(scope, kind, name)` 原地覆盖（`MemoryRecordStore::upsert`），模型没有退役一条记忆的途径；这是不是问题，没有数据。

**不做的事**，以及原因：

- 不给 `MemoryRecord` 加 `status`（candidate / verified / disputed / stale）和验证阶段。能验证的只有模型或宿主；"已验证"这条轴已有 `MemoryTrustLevel` 与 attestation。
- 不恢复整理 run，也不做"可插拔 Consolidator"接口（DEC-13 第 3 条：没有休眠开关）。重开的前提见 4.3。
- 不做跨会话的图存储或专用索引。`MemoryRecord.links` 已存在，没有实测数据支持在它之上再加一层。
- 不在存储里维护 `supersedes` 修订链。旧内容在提出它的那次会话的 `llm_completed.tool_calls` 里，4.1 让它可以被定位。

任务：

- [x] 4.1 **提升时带上证据（R）。**（2026-11-16，提前到阶段 1 之前做：它只需要 `note_evidence` 这个方法，不依赖笔记成为 item） `plan_memory_write` 用 `note_evidence` 同一套解析处理 `evidence_refs`，在 `CanonicalMemoryWrite` 上新增一个可选字段，记录解析出的 item id 与提案所在的 turn；`memory_written` 事件带上同样的字段。解析不出的引用原样保留，不拒绝写入。
  - 模型可见内容不变：工具 schema、工具结果、渲染都不动。
  - 先核实：`llm_completed.tool_calls` 是否带完整参数；四端 SDK 的记忆存储是否原样保存 `provenance`（Node 的 `agent-facade.ts:202` 写入空的 `evidence_refs`）。
  - 验收：kernel golden 的渲染部分逐字节不变；一个测试从一条记忆出发，经 `memory_written` 找到提案的 `llm_completed`，再找到它引用的工具结果；旧 session log 与旧记忆记录仍可加载。
  - 这是对 wire 类型的增量修改，需要 §7 的例外，见 §8 第 5 条。
  - **执行记录。** 核实结果：`llm_completed.tool_calls` 带完整参数；Python 与 Rust 已在记录上写 `provenance.session_id`，Node 与 WASM 没写，从一条存储的记忆连它出自哪个会话都找不到。
    - 实际做法比原计划小：没有新增 wire 字段，也没有改 `memory_written` 事件。内核在 `plan_memory_write` 里把模型的 `evidence_refs` 换成完整列表：模型原样写的引用，接着每个解析到的工具结果 `call:<id>`（模型已直接写了该 call id 的不重复），最后是提案调用本身 `proposal:<call id>`（`ContextManager::memory_evidence_refs`，与 `note_evidence` 共用 `evidence_results`）。四端 SDK 本来就原样拷贝这个字段，所以只需要 Node 与 WASM 补上 `session_id`。解析不到的引用原样保留，不拒绝写入。
    - 这样从一条记录出发：`session_id` → 该会话的日志；`proposal:` → 其中 `tool_calls` 含该 id 的 `llm_completed`；`call:` → `results` 含该 id 的 `tool_completed`。
    - 测试：内核 `a_memory_proposal_carries_resolved_evidence_and_its_own_call`；Node 端到端 `leads from a stored memory back to the turn that proposed it and the result it rested on`（按 id 在会话日志里找到提案轮与证据结果）。模型可见内容不变，golden 没有变化。
- [x] 4.2 **度量记忆维护的开销（只读）。** 不进默认 gate。
  - 基准 harness（`trial.mjs` 的 `memoryTools`）从 `llm_completed` 事件给每个 trial 记 `memory_tools`：`note` / `propose_memory` / `memory` 的调用数、模型轮数、只调用这三种工具的轮数、这些调用的参数字符数、模型写出的全部字符数。所有基准自动带上。
  - `deepstrike-lab` 新增四个指标：`recall_tokens`（recall 注入的索引行 token，取自 lifecycle 的 `items`，旧结果也能算）、`memory_calls`、`memory_turn_share`（整轮只做记忆维护的轮数占比）、`memory_output_share`（模型写出的字符里用于记忆调用的占比）。后三个在旧 trial 上显示为"–"，不是 0。
  - 与原定义的差别：原写"占输入输出 token 的比例"。工具参数进入历史后会被反复发送，按 token 归因需要逐请求拆分，账算不清；改为轮数占比和输出字符占比，直接对应"模型花了多少力气维护记忆"。
  - **对既有结果重新计分（`results/multi-session.jsonl`，每个会话序号 6 个序列）：**

    | 会话序号 | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
    |---|---|---|---|---|---|---|---|---|
    | `notes`：recall token | 0 | 58 | 93 | 123 | 137 | 198 | 202 | 184 |
    | `notes`：累计接受的提案 | 0 | 1.0 | 1.7 | 2.2 | 2.7 | 3.7 | 4.8 | 5.7 |
    | `consolidate`（已删除）：recall token | 68 | 608 | 738 | 784 | 836 | 768 | 804 | 792 |
    | `consolidate`（已删除）：累计接受的提案 | 0 | 4.3 | 9.7 | 14.2 | 20.2 | 26.0 | 34.0 | 40.5 |

    - 注入 token 在第 5 个会话后持平（约 200），不随会话数线性增长，上下文优先计划阶段 9 的那条验收成立。
    - 每个序列只有两条规则，`notes` 到第 7 个会话累计接受了 5.7 条提案，模型每个会话仍在提（0.5–1.2 条）。旧结果只记了接受次数，分不清是同名覆盖还是新增记录；4.3 的 `overwrites` 与 `records` 会分清。
    - 整理 run 让提案数涨到 40 条、注入涨到约 800 token，正确率没有变化。这是它被删除的又一个侧面。
- [x] 4.3 **multi-session 加"漂移"变体（基准）。** 实现并实测（2026-11-15）。
  - `multi-session.mjs --drift K`：从会话序号 K（从 0 起，与 task id 一致）起，序列的第二条规则被序列从未用过的一条取代。没有任何提示，`submit` 的反馈方式不变。漂移前的会话与不带 `--drift` 时任务相同，可以和既有结果配对。
  - oracle：`overwrites`（被接受的提案覆盖了同 kind 同 name 的记录，所有模式都报）、`stale_applied`（漂移之后，答案等于按旧规则算出的值）。每个 trial 保留会话开始时的记录原文（`records`）。
  - 已知局限：答错时的反馈只展示"不该计入"的行，不展示"按旧规则被错误排除"的行。漂移后的恢复因此偏难。
  - **实测**：`--memory notes --drift 4`，10 个序列 80 个会话，`deepseek-flash`，工作区内核（阶段 0）。结果在 `results/multi-session-drift.jsonl`，变体 `phase0-multi-notes-drift4`。用量 1128 万输入 token（853 万命中缓存）、90 万输出 token，比估算（770 万 / 69 万）高约 45%。

    | 会话序号 | 0 | 1 | 2 | 3 | 4（漂移） | 5 | 6 | 7 |
    |---|---|---|---|---|---|---|---|---|
    | 正确 / 10 | 2 | 6 | 6 | 7 | 0 | 2 | 2 | 5 |
    | `stale_applied` | – | – | – | – | 0.9 | 0.6 | 0.5 | 0.1 |
    | 会话开始时的记录数 | 0 | 0.9 | 1.7 | 2.3 | 2.8 | 3.4 | 4.2 | 4.9 |
    | 提案 / 其中覆盖旧记录 | 0.9 / 0 | 0.8 / 0 | 0.7 / 0.1 | 0.7 / 0.2 | 1.0 / 0.4 | 1.1 / 0.3 | 1.0 / 0.3 | 1.0 / 0.1 |
    | `memory` 调用 | 11.1 | 4.0 | 6.6 | 2.0 | 1.7 | 3.3 | 2.8 | 2.3 |
    | recall 注入 token | 0 | 50 | 109 | 147 | 175 | 216 | 268 | 293 |

    1. **漂移后三个会话没有恢复。** 漂移前 7/10，漂移当场 0/10（其中 9 个答案正是旧规则的值），之后 2、2、5。
    2. **模型不再套用旧规则，但也没学会新规则。** `stale_applied` 三个会话内从 0.9 降到 0.1，正确率只回到 5/10。失败的主因不是"沿用旧知识"，而是存储里的记录互相矛盾。
    3. **记忆只增不改。** 72 条被接受的提案里只有 14 条（19%）覆盖了旧记录，其余都另起一个名字。到最后一个会话，每个序列有 3–7 条记录描述同一件事，结论相反（例："只排除 canary" 与 "只排除 synthetic-probe" 并存），还有凭空造出的规则（"按 0.75 × 原始计数估算"、"去重"、"每个文件的 00:00 行不计"）。漂移前就已如此，漂移只是让它更糟。
    4. **原因在内核的呈现，不在模型。** `propose_memory` 的说明要求"先搜索，复用已有 name 来更新"，但模型看不到 name：recall 索引行在有 description 时只显示 `record_id`、`kind`、description（`manager.rs` 的 `recall_index_line`），搜索与点读的结果是 `[MEMORY record_ref=… kind=…] <content>`（`driver/effects.rs`）。19% 的覆盖率是模型碰巧起了同一个 slug。
    5. **记忆维护占了 37.5% 的模型轮次**（`memory_turn_share`：885 轮里 332 轮只调用了记忆工具）。第 0 个会话存储是空的，模型平均仍搜索 11.1 次。`memory_output_share` 为 62.5%，但这个任务的答案只是一个数，这个比例本身不说明问题。
    6. **作废的 oracle。** 原设计的 `stale_records` 与 `records_conflicting` 靠关键词判断记录是否提到某条规则。实测中几乎每条记录都提到每条规则（"canary 行**要**计入"），关键词分不清肯定与否定，数值无意义，已从基准删除；结果文件里这两个字段不要读。
    7. **基准自身的问题。** `retry` 规则（req 以 0 结尾）在 10 个序列里从未被任何一条记录提到过：模型从三行示例里看不出它。以它为初始规则的 SEQ#6 八个会话全错；以它为漂移后新规则的 5 个序列，漂移后 15 个会话对了 4 个，其余 5 个序列对了 5 个。样本太小，不能说 `retry` 拖累了恢复，但后续实验应记下这一点。
    8. **协议层错误 2 / 80**：`OpenAI-compatible replay has assistant tool_calls with no tool result`（SEQ#2@1、SEQ#3@5）。0.2.82 的 `notes` 结果里也有 1 / 48，不是阶段 0 引入的。与 0.2.80 修过的"记忆命中插进工具批次"是同一类报错，需要单独查。
- [x] 4.4 **按 4.3 的结果决定（G）。** name 进入头部，已成为默认；排查出的缺陷都已修复（见 §10.2）。
  - 4.3 的结论落在"不能自己修复"一侧，但原定的候选机制（让模型退役一条记忆）不是第一步：模型连已有的更新途径都用不上，因为它看不到 name。
  - **候选改动：让模型看到 name。** recall 索引行、搜索结果、点读结果的头部都带上 `name=<slug>`。不加工具、不加动作、不加 effect。它改变模型看到的内容，按 DEC-13 走 gate：放在默认关闭的实验开关后，对"没有它的配置"实测。
    - 场景：multi-session `--drift 4`（目标场景，看正确率、`overwrites` 占比、记录数）与不带漂移的 multi-session（不能变差）。先各 10 个序列看方向，再补到 30 对以上。
    - 每个会话的索引行会多几个 token，属于成本一侧，由 `cost_per_success` 一并裁决。
  - **实测（2026-11-15 至 16）。** 实验开关 `DEEPSTRIKE_EXP_MEMORY_NAME=1`（`ContextManager::exp_memory_name`，环境变量，仅用于实验，不发布）。两组都跑在修复协议层错误之前的内核上，与各自的基线条件一致。结果：`results/multi-session-drift.jsonl` 的 `exp-name-drift4`、`results/multi-session-name.jsonl` 的 `exp-name`。

    | | 漂移：基线 | 漂移：带 name | 无漂移：基线（0.2.82） | 无漂移：带 name |
    |---|---|---|---|---|
    | 会话数（序列数） | 80（10） | 80（10） | 48（6） | 48（6） |
    | 正确 | 30 | 37 | 33 | 33 |
    | 其中漂移后（会话 4–7） | 9 / 40 | 14 / 40 | – | – |
    | 提案 / 其中覆盖旧记录 | 72 / 14（19%） | 74 / 61（82%） | 39 / 0 | 32 / 25（78%） |
    | 最后一个会话开始时的记录数 | 4.9 | 1.3 | 5.7 | 1.2 |
    | 最后一个会话的 recall token | 293 | 111 | 184 | 101 |
    | `weighted_cost`（每会话，相对单位） | 90277 | 80265（−11%） | 73231 | 65873（−10%） |
    | 输入 / 输出 token（每会话） | 141k / 11.3k | 119k / 9.4k | 96k / 8.6k | 97k / 6.9k |
    | 只做记忆维护的轮次占比 | 37.5% | 34.6% | –（旧 trial 无此项） | 30.6% |
    | 运行错误 | 2 | 4 | 1 | 1 |

    - **机制层面的变化是确定的**：模型看到 name 后就用它更新。覆盖率从 19% 到 82%，存储从每序列约 5 条互相矛盾的记录变成 1 条，注入量不再随会话数增长。
    - **gate**（`deepstrike-lab gate`，DEC-13 默认规则）：漂移场景 PASS（有一项显著变好，没有一项显著变差）。无漂移场景按默认规则 FAIL（"没有一项显著变好"，33/48 对 33/48）；它不是目标场景，按"不能变差"检查（`--improve none`）PASS。
    - **证据的弱点，如实记录**：
      1. 80 对会话来自 10 个序列，同一序列的会话相互不独立（记忆是累积的）；配对 bootstrap 把它们当作独立样本，区间偏窄。按序列看：10 个序列里 7 个变好、2 个变差、1 个持平。9.4 当时也按会话计数，口径一致，但这是口径的弱点，不是这次才有。
      2. 漂移后的恢复仍然不好：14 / 40。name 解决的是"记录只增不改"，没有解决"模型从三行示例里学不会新规则"。
      3. 无漂移的基线是 0.2.82 内核的旧结果，依据是阶段 0 不改变模型可见内容。
      4. 候选组的 4 个运行错误里有 4 个是已修复的那条协议层错误，都计入了候选组的失败，结论因此偏保守。
    - **结论：通过，已成为默认（用户 2026-11-16 确认）。** 环境变量开关已删除；三处头部固定带 `name=`（`ContextManager::memory_name_field`，记录没有 name 时不带）；`memory` 工具说明里的头部格式同步更新。四端 SDK 都不自己拼这三种头部，只改了断言旧格式的测试（内核 3 处、Rust SDK 2 处、Node 3 个文件、WASM 2 个、Python 3 个）。kernel golden 没有变化。写入 CHANGELOG 与 `MIGRATION-0.2.83-to-0.2.84.md`。验证：`cargo test --workspace`、Node 1421 个测试、WASM `test:canonical-binding` 全部通过；Python 的 venv 失效，三个改过断言的测试文件没有跑过。
  - 只有在模型看得到 name 之后记录仍然只增不改，才考虑退役动作；离线整理排在其后，且必须在漂移变体上实测。
  - **协议层错误的排查结果（2026-11-16）。** 用脚本化 provider 离线复现（随机批次 80 次运行 + 约 40 个定向用例），四个问题都已修复，见 §10.2：
    - `propose_memory` 的结果消息夹在 assistant 的调用和它的结果之间（实测里的那条报错）；
    - `memory` 与 `propose_memory` 同一轮，四端 runner 都会让运行失败；
    - 重复熔断在同一轮的记忆查询未结清时就调用 provider；
    - 调用未暴露的内核工具以 `invalid_authority` 终止运行。
  - 另记一个不属于本项、但被这次数据指出的问题：上面第 5 点（空存储上的反复搜索）。先看 name 改动之后的数字再说。
- [ ] 4.5 **候选实验，不排期：原始记录检索作为模型工具。** `transcript_search` 只作为整理 run 的一部分测过，没有单独测过"模型在会话里自己查过去的日志"。等 4.3 的基准就绪后再决定是否值得一测。

## 6. 顺序与版本

```text
阶段 0 ──► 阶段 1（1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.6 → 1.7 → 1.8 → 1.9）──► 阶段 3
              └──► 阶段 2 可在 1.4 之后开始（实验开关依赖形态合一后的降级路径）

阶段 4：4.2、4.3 与阶段 1 无依赖，可随时开始；4.1 在 1.2 之后（笔记成为 item 后，两处证据解析合一）；4.4 在 4.3 之后
```

- 阶段 0 与阶段 1 不改变模型看到的内容，可以合成一个版本发布。4.1 与 4.2 同样不改变，可以并入。
- 阶段 2 的删除会改变渲染，单独一个版本，带 MIGRATION。4.4 若引入退役动作，同样单独发布。
- 发布规则不变：release PR 先合入 main，再给合并提交打 tag。

## 7. 约束

- DEC-13 全部条款继续有效。本计划不新增 effect、origin、relation。新增 lease 种类只允许 1.1 的那一种，并且以替换 `SingleUse` 为前提。
- 阶段 0–1 的任何 PR，只要 golden 的渲染部分发生变化，就不是重构：停下来找原因，不要 bless。
- 不为了统一而统一。合并两个概念的前提是它们回答同一个问题；`CapabilityLease`、`MemoryScope`、`RepeatFuse` 属于 effect 准入，留在原处。
- 不动 `runtime/kernel/wire` 的 ABI 类型、journal、restore（唯一的例外是 4.1 的可选字段，用户已放行，见 §8 第 5 条）。`runtime/` 有约 31k 行非测试代码，是 core 最大的部分，值得单独一轮审计，不在本文范围。
- 内核不读时钟、不发起模型调用（D1、DEC-13 第 1 条）。

## 8. 需要用户决定

1. 阶段 2 的实测预算上限。
2. `EntropyWatchConfig.notify_model`（默认关闭）是否按"没有休眠开关"删除。它是给宿主监督者之外的第二条通道，没有实测数据。
3. 1.6 列出的"现有行为与写入权表冲突"的条目，逐条裁决。
4. 阶段 1 改变 checkpoint 形状。按此前"允许破坏式更新、写 MIGRATION"的约定执行；如需保持旧 checkpoint 可恢复之外的更强兼容，请说明。
5. ~~4.1 给 `CanonicalMemoryWrite` 与 `memory_written` 事件加可选字段，触及 §7"不动 wire ABI 类型"的约束。~~ 用户 2026-11-15 裁决：放行。4.1 排在 1.2 之后。
6. 4.3 的实测预算上限（与第 1 条分开计）。

## 9. 执行须知

- 环境准备见上下文优先计划的"交接"一节：重建 napi addon 与 `node/dist` 后再跑 Node 测试和基准；`.env` 需要 `DEEPSEEK_API_KEY`、`DEEPSEEK_MODEL`；WASM 测试需要 rustup 工具链；Python 用独立 venv。
- golden 重新生成：`BLESS_KERNEL_RECORD_FIXTURES=1 cargo test -p deepstrike-core --lib <测试名>`。阶段 0–1 原则上不应需要它。
- 每个 PR 跑：`cargo test --workspace`、Node / Python / WASM 测试、四端 conformance。
- 复现 0.2.82 之前行为的基线必须显式传 `notesBudgetTokens: 0`。
- gate：`deepstrike-lab gate` 不带参数即 DEC-13 规则。
- 本文 §2 标了"需核实"的结论，以及所有"无使用"的判断，动手前用代码确认；与本文不符时以代码为准，并更新本文。

## 10. 交接（2026-11-16）

接手者从这里读起。分支 `refactor/kernel-distill-phase0`，基于 `main`（`df26eabf`，v0.2.83），未推送，未开 PR。

### 10.1 已完成，都在这个分支上

| 提交主题 | 内容 |
|---|---|
| 阶段 0 清理 | §5 阶段 0 的 0.1–0.7：删除 `asyncSummarizer`、`summary_upgraded`、无人读取的配置；两个 compactor 合一 |
| 基准与 lab | 4.2 的四个记忆维护指标；4.3 的 `multi-session --drift`；实测结果文件 |
| 协议层修复 | `propose_memory` 结果消息的顺序（内核）；`memory` + `propose_memory` 同轮（Node runner）；复现脚本 `scripts/kernel-tool-batches.mjs` |
| name 进入记忆头部 | 4.4，默认开启；CHANGELOG、MIGRATION、文档 |
| 计划 | 本文 |

### 10.2 待办，按顺序

1. **两个缺陷。**
   - [x] 重复熔断与 `memory` 同轮（2026-11-16，`dfcb62fb`）。根因：熔断或门禁用合成结果回答本轮宿主调用时，引擎立刻调用 provider，不管同一轮的 syscall 是否已发布内核工作（记忆查询或写入）；一个 step 同时发布 `QueryMemory` 与 `CallProvider`，下一次请求与查询赛跑，渲染时没有查询结果。修复：这种情况下合成提交返回 `AwaitingResume`，工作结清后经 `resume_after_preload` 继续，与未熔断的轮次一致（`state_machine/mod.rs` 的 `turn_awaits_kernel_work` / `defer_provider_call`，`gate.rs` 的 `commit_synthetic_results`）。内核回归测试 `a_fused_turn_waits_for_the_memory_query_it_made`，去掉修复即失败。
   - [x] **未暴露的内核工具终止运行**（2026-11-16，`dd6d11e4`）。用户的裁决："先问为何没有暴露对应的内核工具，这才是核心；调用没有的工具应返回错误，让模型去找真实的工具，多次无果进入总结。"
     - **为何没有暴露。** `read_result` 只在有工具结果离开上下文时才提供。page-in 让结果回到上下文，压缩或 renewal 时 `reset_collapse_generation` 也让折叠的结果回来；最后一个离开的结果回来后，工具就从下一轮的工具表里消失，而模型的历史里刚刚用过它，工具说明还让它用 `offset` 分页读大结果。golden `golden_lifecycle_external_payload` 原本就记录了这一点：page-in 之后的请求里没有 `read_result`。修复：提供过一次就一直提供（`ContextManager::read_result_offered`，进 checkpoint）。工具表不再缩小，也少了一类缓存前缀断点。
     - **调用没有的工具。** 驱动不再以 `invalid_authority` 终止，而是像未暴露的宿主工具一样回错误结果（"Tool '…' is not available on this turn. Choose from the tools listed in your tools schema."），不执行。这类调用计入重复熔断，元工具名也不豁免：重复 5 次拒绝，8 次进入总结轮后以 `NoProgress` 结束。终止档的总结轮本身不提供工具，模型在这一轮调用工具，运行照常以 `NoProgress` 结束。
     - 测试：`a_tool_the_turn_never_exposed_has_no_caller_to_derive`（submit 路径）、`repeated_calls_to_an_unexposed_kernel_tool_burn_the_repeat_fuse`、重新生成的 golden，去掉修复均失败。`scripts/kernel-tool-batches.mjs` 117/117，三个随机种子各 40 次运行零失败。四端全量测试通过。
     - 这两项改变了模型在出错路径上看到的内容，按缺陷修复处理，没有过 DEC-13 的 gate；写进了 CHANGELOG 与 MIGRATION。
2. **补上一轮留下的缺口。**
   - [x] Node runner 的回退补了回归测试（`b97c9c13`），去掉修复即失败。
   - [x] WASM、Python、Rust 的 runner 有同样的问题，已用同一种回退修复，各加一个回归测试，去掉修复即失败（`b3f012b0`）。
   - [x] Python：旧 venv 指向已不存在的 Python 3.11。在临时目录用 Python 3.12 新建 venv、`maturin develop --release` 后全量 pytest 通过（913 passed）。本机 shell 设了 SOCKS 代理，跑 pytest 要先清掉 `ALL_PROXY` / `HTTP(S)_PROXY`，否则 httpx 报缺 `socksio`。
   - [x] 英文版 `docs/en/plans/kernel-distillation.md`（`check-docs-drift` 要求中英文成对）。中文版是工作副本，先改中文再同步英文。
3. **阶段 1（1.1–1.9）**，然后 **4.1**（排在 1.2 之后，wire 可选字段已获放行）。
4. ~~**阶段 2 的 DEC-13 审计。**~~ 第一轮已完成，M1–M7 已删除（见阶段 2 的执行记录）。M8、M9 待定。
5. **阶段 3 文档。**

### 10.3 这一轮学到的，动手前要知道

- **读数据再下结论，不要读 oracle 的数字就下结论。** 4.3 设计的两个关键词 oracle 数值全部无效，是直接读记录原文才发现的；4.4 的候选机制也因此从"退役动作"换成了"显示 name"。阶段 2 每个去留都要打开 trial 看几条原始记录。
- **按会话配对的样本不独立。** multi-session 的 80 对来自 10 个序列。报告时同时给出按序列的胜负数。
- **漂移后的恢复仍然不好**（14 / 40）。name 解决了"记录只增不改"，没有解决"模型学不会新规则"。`retry` 规则（req 以 0 结尾）在全部运行里没有被任何记录提到过，基准的这条规则可能太难；改基准之前先和用户确认，因为它会让新旧结果不可比。
- **记忆维护占三成以上的模型轮次**，第 0 个会话在空存储上平均搜索十次左右。没有处理，只有数据（4.2、4.3）。这是阶段 2 之外最明显的成本项。
- **实测花费会超估算。** 4.3 的首轮超了约 45%。报预算时留余量，并且查不了余额，要请用户确认。

### 10.4 操作上的坑

- 本机没有 `napi` CLI。重建 addon：`cargo build --release -p deepstrike-node`，然后把 `target/release/libdeepstrike_node.dylib` 复制为 `crates/deepstrike-node/deepstrike-core.darwin-arm64.node`。绑定层接口（`crates/deepstrike-node/src`）变了的话，这样做不会重新生成 `index.js` / `index.d.ts`，需要装 `@napi-rs/cli` v2。
- 有基准在后台跑的时候，不要原地覆盖 `.node` 文件，也不要 `npm run build`（它先删 `node/dist`）。替换 addon 用"复制到临时名再 `mv`"。
- zsh 下 `echo ====X`、未加引号的 `--include=*.rs` 会报错；路径通配加引号。
- 不要把别的命令接在测试命令后面用 `&&` 串起来图省事。上一轮因此误执行过一次 `git stash`。
