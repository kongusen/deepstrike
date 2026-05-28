# Context & Runtime Optimization — v3 Spec

**Status:** Partially shipped — four-slot refactor, compression log, renderer fixes, and sub-agent harness integration are in tree. P0 token-counting and Anthropic prompt caching tracked below.  
**Target version:** 0.3.0  
**Scope:** `deepstrike-core` kernel + Node SDK (providers, runner) + tokenizer crate + WASM  
**Predecessor:** [`spec-context-compression-v2.md`](spec-context-compression-v2.md) (six-partition model — superseded)  
**Current architecture:** [`context-partition-compression.md`](context-partition-compression.md) (four-slot model)

> 本文档的优先级排序是关键产出。`v0.1.16 → v0.2.2` 之间 kernel 从轻量状态机升级为 Agent OS runtime,引入了 context VM、压缩/归档、JSON ABI、task_state/dashboard 注入、事务/rollback/milestone/capability/sub-agent。体感「变慢、变差」的真正主因**不是** tiktoken,也不只是 oldest-first,而是下面 P0 的两个测量/缓存问题。

### Shipped since this spec was drafted

| Item | Status | Notes |
|------|--------|-------|
| Four-slot context model | ✅ | `system_stable`, `system_knowledge`, State turn, `history` — see [context-partition-compression.md](./context-partition-compression.md) |
| `compression_log` unified routing | ✅ | All four tiers append `CompressionEntry`; Collapse/Auto summaries render via `format_compact()` → Slot 3 |
| `preserve_recent_turns` from config | ✅ | No hardcoded 4 in CollapseCompactor/AutoCompactor |
| Renderer: `normalize_turn_prefix` | ✅ | `[context resumed]` anchor when preserved tail lacks user turn |
| Renderer: signals → Slot 3 | ✅ | Replaces `working` partition; new user turn when first history msg is Parts |
| Renewal carryover | ✅ | system + knowledge + task_state; signals cleared; scratchpad cleared |
| `initialMemory` → Slot 2 | ✅ | `add_knowledge_message` replaces deleted `add_memory_message` |
| Sub-agent × HarnessLoop | ✅ | `RuntimeOptions.subAgentHarness` / `sub_agent_harness` → criteria from `AgentRunSpec.milestones` |
| P0-1 token counting fix | 🔄 | See § P0-1 below |
| P0-2 Anthropic prompt caching | 🔄 | Slot 1/2 breakpoints implemented; full acceptance in § P0-2 |
| P0-3 renderer recency-biased | ✅ | Newest-first history fill |
| P0-4 goal dedup | 🔄 | See § P0-4 below |

> 本文档的优先级排序是关键产出。`v0.1.16 → v0.2.2` 之间 kernel 从轻量状态机升级为 Agent OS runtime,引入了 context VM、压缩/归档、JSON ABI、task_state/dashboard 注入、事务/rollback/milestone/capability/sub-agent。体感「变慢、变差」的真正主因**不是** tiktoken,也不只是 oldest-first,而是下面 P0 的两个测量/缓存问题。

---

## 0. 现状校正(相对你最初的分析)

写方案前先纠正几个前提,避免在错的地方投入:

| 你的判断 | 核实结论 | 证据 |
|---|---|---|
| tiktoken 在 kernel,应移出 | **方向对,但它是编译期硬依赖**,不是运行期默认用。默认引擎是 `char_approx`,但 `tiktoken-rs` 的 BPE 表无条件编进二进制(WASM 受害最大) | [`deepstrike-core/Cargo.toml:14`](../crates/deepstrike-core/Cargo.toml#L14) 无 feature gate;`tiktoken-rs` 只有 OpenAI 分词器,对 Claude 本就是近似 |
| renderer oldest-first 会挤掉最新 turn | **真 bug,但条件触发**。压缩正常时 history 已 < budget,全装得下;只有 preload 大历史 / system 过大 / 单条超预算时暴露 | [`renderer.rs:69-94`](../crates/deepstrike-core/src/context/renderer.rs#L69-L94) |
| 规则压缩丢语义,需做 Phase D | **Phase D 大部分已实现**。Snip 已 head+tail,Micro 已 JSON 摘录+分级(200/2000 token)+`preserved_refs` 白名单,Collapse/Auto 已保留最近 2 轮 + 归档。**唯一缺口是 LLM 语义摘要** | [`compression.rs`](../crates/deepstrike-core/src/context/compression.rs) 全文 |
| state_machine.rs 是旧的 deprecated 代码 | **它就是真身**。`KernelRuntime` 只是 `sm: LoopStateMachine` 的薄包装,deprecated 指「别从外部直驱」 | [`kernel.rs:403-405`](../crates/deepstrike-core/src/runtime/kernel.rs#L403-L405) |
| (未提及) | **🔴 token 计数有放大 bug**:assistant 消息的 `tokenCount` 被设成整个 prompt 的 `input+output`,rho 按每条消息求和 → 历史 token 被 O(n²) 高估,过早/反复压缩 | [`anthropic.ts:132`](../node/src/providers/anthropic.ts#L132) → [`runner.ts:529`](../node/src/runtime/runner.ts#L529) |
| (未提及) | **🔴 全链路无 prompt caching**,且动态 system 前缀会让缓存即便加上也每轮失效 | `grep cache_control node/src` 为空;[`anthropic.ts:72`](../node/src/providers/anthropic.ts#L72) |

---

## 1. 根因总览

| # | 类别 | 症状 | 根因 | 优先级 |
|---|---|---|---|---|
| 1 | 测量 | 过早/反复压缩,上下文被无谓销毁 | assistant `tokenCount = input+output`,rho 求和高估 | **P0** |
| 2 | 延迟/成本 | 每轮全量重算,慢且贵 | 无 prompt caching + 动态 system 前缀破坏可缓存性 | **P0** |
| 3 | 质量 | 偶发丢最新 user turn | renderer oldest-first 填预算 | **P0** |
| 4 | 噪声 | goal 重复、管理文本喧宾夺主 | goal 同时进 system_text 与 user message | **P0** |
| 5 | 体积/依赖 | WASM 臃肿,Claude 计数不准 | tiktoken 编译期硬依赖 | **P1** |
| 6 | 质量 | 压缩摘要语义损失 | 仅 `RuleSummarizer`,无异步 LLM 摘要 | **P1** |
| 7 | 噪声 | 模型拘谨、轮次变多 | rollback/dashboard 默认注入 prompt | **P1** |
| 8 | 延迟 | AutoCompact 时尖刺 | `archived: Vec<Message>` 全量过 FFI;per-step JSON 编解码 | **P2** |

---

## P0 — 立刻见效、低风险

### P0-1. 修复 token 计数放大(rho 元凶)

**问题.** assistant 消息被赋予整轮 prompt 的 token 数,而 rho 又对每条消息求和,导致每多一轮就把「当前整 prompt 体积」再次累加进 history.token_count。第 n 轮的 assistant 携带 ≈ n × 单轮增量,history 计数呈 O(n²) 增长,rho 远高于真实占用 → 压缩/renew 被过早、反复触发,真实可用上下文反而被销毁。这是「变差」最可能的单点主因。

**现状.**
- Provider 把整轮用量写进单条消息:`tokenCount: resp.usage.input_tokens + resp.usage.output_tokens`([`anthropic.ts:97`](../node/src/providers/anthropic.ts#L97)、流式 [`:132`](../node/src/providers/anthropic.ts#L132))。
- Runner 原样回传:`assistantMessage.tokenCount = turnTokens`([`runner.ts:525-530`](../node/src/runtime/runner.ts#L525-L530))。
- Kernel 消费:`message_tokens()` 用 `message.token_count`,push 进 history([`state_machine.rs:318-351`](../crates/deepstrike-core/src/scheduler/state_machine.rs#L318-L351));rho = 各分区 `token_count` 之和 / `max_tokens`([`pressure.rs:32`](../crates/deepstrike-core/src/context/pressure.rs#L32))。

**方案.** 分两步,先止血再做对。

*Step 1 — 止血(SDK 侧):* assistant 单条消息只记 `output_tokens`,不含 `input_tokens`。
```ts
// anthropic.ts complete() / stream()
// 区分 input/output,消息只携带 output
const message = {
  role: "assistant" as const,
  content,
  tokenCount: resp.usage.output_tokens,        // ← 只记产出
  toolCalls,
}
// 另把整轮 usage 单独透出,供 Step 2 使用
```
对所有 provider adapter([`base.ts`](../node/src/providers/base.ts) 及各家)统一:消息级 `tokenCount` 永远是「该消息自身」的 token,不是整轮 prompt。

*Step 2 — 做对(Claude Code 式权威计数):* provider 每轮回传的 `input_tokens` 就是「当前整个 prompt 的精确 token 数」,直接拿它当 rho 分子,彻底摆脱「逐条估算求和」的累积误差。
- 扩展 ABI:`ProviderResult` 事件携带 `usage`。
  ```rust
  // kernel.rs KernelInputEvent
  ProviderResult {
      message: Message,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      observed_input_tokens: Option<u32>,   // provider 报告的整 prompt token 数
      #[serde(default, skip_serializing_if = "Option::is_none")]
      observed_output_tokens: Option<u32>,
  }
  ```
- Kernel 保存 `last_observed_prompt_tokens`,`rho()` 优先用它:
  ```rust
  // pressure.rs — 有权威观测值时用观测值,否则回退到逐条求和(估算)
  pub fn rho(&self) -> f64 {
      match self.last_observed_prompt_tokens {
          Some(obs) => obs as f64 / self.max_tokens as f64,
          None => self.estimated_rho(),   // 现有逐条求和路径,仅作 pre-flight 兜底
      }
  }
  ```
- 逐条 `token_count` 估算降级为「下一次 LLM 调用前的预判 / 截断决策」用途,不再是 rho 的唯一来源。

**Claude Code 对照.** 预算判定以 API 实际 `usage.input_tokens` 为准;客户端 tokenizer 仅做发送前的粗略预估。

**验收.**
- 单测:连续 10 轮、每轮固定增量,`rho` 增长应近似线性而非二次;assistant 消息 `tokenCount` ≤ 单轮 `output_tokens`。
- 集成:同一长会话在修复前后,压缩触发次数显著下降(记录 `Compressed` observation 计数对比)。
- 兼容:`observed_input_tokens` 缺省时(老 SDK / 非主力 provider)回退逐条求和,行为不变。

**风险.** 低。改的是「数值来源」,不改控制流。需同步各 provider adapter 的 usage 透出。

---

### P0-2. 启用 prompt caching + 稳定可缓存前缀

> **实现校正（四槽重构后）.** 原 spec 提议 `system_volatile` 作为独立字段。当前实现将 `task_state` + `signals` 渲染进 `turns[0]`（State turn），`RenderedContext` 暴露 `system_stable` + `system_knowledge` + `turns`。缓存断点打在 Slot 1/2，State 层每轮重建但不污染前缀 — 效果等同原 intent。

**问题.** 当前每轮把完整 `system + tools + 全部 history` 当作未缓存内容重新发送 / 重新计费。这是「变慢变贵」的最大单点。即便加上缓存,只要把每轮都变的 `task_state.progress` / `dashboard` 拼在 system 前缀里,缓存断点每轮失效,等于没加。

**现状.**
- 无任何 `cache_control`(`grep` 为空);request 直接 `system = context.systemText`([`anthropic.ts:72`](../node/src/providers/anthropic.ts#L72)、[`:110`](../node/src/providers/anthropic.ts#L110))。
- `system_text = system 分区 + task_state + dashboard` 拼接([`renderer.rs:18-36`](../crates/deepstrike-core/src/context/renderer.rs#L18-L36)),其中 `task_state.progress` 每轮被工具结果更新 → 整块每轮变。

**方案.** 两件事必须一起做,缺一无效。

*(a) 拆分稳定 / 易变内容.* 让 `RenderedContext` 区分「可缓存前缀」与「易变尾部」:
```rust
// renderer.rs RenderedContext
pub struct RenderedContext {
    pub system_stable: String,   // 安全规则/契约/能力清单:整会话稳定 → 可缓存
    pub system_volatile: String, // task_state / dashboard / progress:每轮变 → 不进缓存前缀
    pub turns: Vec<Message>,
}
```
- `system_stable` = 现 system 分区(规则)+ 不变的能力清单。
- `system_volatile`(task_state + dashboard)**移出 system 前缀**,改为「附加到最后一个 turn 之后的一条 system-role 消息」(Claude Code 的 system-reminder 模式),位于缓存断点之后,不影响前缀命中。

*(b) 打缓存断点.* anthropic adapter 对稳定部分加 `cache_control`:
```ts
// anthropic.ts buildTools(): 末尾工具打断点(tools 稳定)
const tools = this.buildTools(toolSchemas)
if (tools.length) tools[tools.length - 1].cache_control = { type: "ephemeral" }

// system 用 block 数组,稳定块打断点
const system = systemStable
  ? [{ type: "text", text: systemStable, cache_control: { type: "ephemeral" } }]
  : undefined

// 增量缓存:对倒数第二个 user turn 再打一个断点,让历史前缀逐步进缓存
```

**Claude Code 对照.** system prompt + tools 字节级稳定 → 长期缓存命中;待办/状态以 system-reminder 形式追加在对话尾部,不污染缓存前缀。

**决策反转.** 这推翻 v2 spec §10 决策 #3(task_state 渲染进 system_text)。理由:缓存收益远大于「放 system 前缀」的语义整洁度。task_state 仍是 SSOT、仍每轮渲染,只是渲染**位置**改到缓存断点之后。

**验收.**
- 第 2 轮起 Anthropic 响应 `usage.cache_read_input_tokens > 0`,且占比随轮次上升。
- 改造后 `system_stable` 在一次 run 内逐字节不变(单测做哈希断言)。
- 端到端:多轮会话平均首 token 延迟与输入计费 token 显著下降(记录基线对比)。

**风险.** 中。各家缓存语义不同 → **先只做 Anthropic**;其它 provider 保持原样,靠 `system_stable`/`system_volatile` 字段兼容(不支持缓存的把两者拼回即可)。

---

### P0-3. Renderer 改「最近优先 + 永不丢最新 turn」

**问题.** 预算不足时,最新的 user 目标可能被截断或丢弃,模型答非所问。

**现状.** [`renderer.rs:69-94`](../crates/deepstrike-core/src/context/renderer.rs#L69-L94) 从 `history.messages[0]`(最旧)正序填预算,`else { break }` 直接丢弃尾部(最新)。注意压缩侧 Collapse/Auto **已经**保留最近 2 轮([`compression.rs:296-297`](../crates/deepstrike-core/src/context/compression.rs#L296-L297)),render 与 compress 方向相反,不一致。

**方案.** render 改为 recency-biased,与压缩对齐:
1. 反向遍历 `history`(最新 → 最旧),累加预算;
2. 强制保留最近 K 轮(`config.preserve_recent_turns`,默认 2~3),最新 user turn 绝不截断;
3. 预算耗尽时丢/截**最旧**的,而非最新的;
4. 收集完再正序输出,保持 user/assistant/tool 严格交替。

```rust
// renderer.rs render() 核心改写
let mut kept_rev: Vec<Message> = Vec::new();
for msg in partitions.history.messages.iter().rev() {
    let tokens = msg.token_count.unwrap_or_else(|| engine.count_message(msg));
    if tokens == 0 { continue; }
    let within_protected = kept_rev.len() < config.preserve_recent_msgs;
    if tokens <= remaining || within_protected {
        kept_rev.push(msg.clone());
        remaining = remaining.saturating_sub(tokens.min(remaining));
    } else if remaining > 0 {
        // 截最旧的那条(此处是即将丢弃的更旧消息),而非最新
        if let Content::Text(_) = msg.content {
            kept_rev.push(engine.truncate_message(msg, remaining));
        }
        break;
    } else { break; }
}
kept_rev.reverse();
let turns = kept_rev;
// 注意:反向保护可能引入首条非 user,需要做开头规整(丢弃悬空 assistant/tool 开头)
```

**Claude Code 对照.** 保留最近上下文是硬不变量;最新用户指令永不被压缩逻辑挤掉。

**验收.**
- 新单测:history 远超 budget 时,最新 user turn 必在 `turns` 中且未截断。
- 改写后仍满足现有 `no_consecutive_user_messages_with_signals`、`text_truncated_when_budget_exhausted`(需调整断言为「截断最旧」)。
- 开头不出现悬空的 assistant/tool 起始(provider 会报错)。

**风险.** 低~中。交替性与开头规整是易错点,需充分测试。

---

### P0-4. 去重 goal,降低管理性文本

**问题.** goal 同时出现在 `[TASK STATE]`(system_volatile)和首条 user message,重复占 token、且让模型在「管理叙述」与「真实指令」间分心。

**现状.** [`state_machine.rs:270-296`](../crates/deepstrike-core/src/scheduler/state_machine.rs#L270-L296):`init_task()` 写 task_state,随后又把 `goal + criteria` 拼成 user message push 进 history。

**方案.**
- task_state 已是 SSOT(渲染在 system_volatile)→ 首条 user message 不再重复 goal 全文。
- 二选一:(a) 首条 user message 仅放「真正要模型现在做的指令」短句;或 (b) 完全不造 user message,首轮直接以 task_state 驱动(需确认 provider 接受无 user turn 的首轮 → 多数不接受,故倾向 (a),user message = 精简指令)。
- criteria 归入 task_state 的 `criteria` 字段渲染,不再进 user message。

**验收.** 首轮渲染中 goal 仅出现一次;现有 `start_places_user_message_in_history_not_working` 等测试同步更新。

**风险.** 低。

---

## P1 — 中等,需要设计

### P1-5. tiktoken 降级为可选 feature + 计数权威化

**问题.** `tiktoken-rs` 的 BPE 合并表无条件编进 `deepstrike-core` → 所有下游(尤其 WASM)体积膨胀;且它只覆盖 OpenAI,对 Claude 是近似,价值有限(P0-1 已让权威计数来自 provider usage)。

**现状.** 硬依赖 [`Cargo.toml:14`](../crates/deepstrike-core/Cargo.toml#L14);`ContextTokenEngine::cl100k()/o200k()` 实例化 tiktoken([`token_engine.rs:67-77`](../crates/deepstrike-core/src/context/token_engine.rs#L67-L77));`SetTokenizer` 事件切换([`kernel.rs:448-455`](../crates/deepstrike-core/src/runtime/kernel.rs#L448-L455))。

**方案.**
- `deepstrike-core` 加 cargo feature:
  ```toml
  [features]
  default = []
  tiktoken = ["dep:deepstrike-tokenizer"]
  [dependencies]
  deepstrike-tokenizer = { workspace = true, optional = true }
  ```
- `token_engine.rs` 用 `#[cfg(feature = "tiktoken")]` 包裹 `cl100k()/o200k()` 与 `TiktokenCounter`;无 feature 时 `SetTokenizer` 的 tiktoken 分支回退 `char_approx`(已是 [`kernel.rs:452`](../crates/deepstrike-core/src/runtime/kernel.rs#L452) 的 default arm,天然兼容)。
- WASM crate **不**开 `tiktoken`;Node/Python SDK 需要精确 OpenAI 计数时再开,或干脆 SDK 侧用 JS `tiktoken` 注入。
- rho 已由 P0-1 改用 provider usage;tiktoken 仅服务「无 usage 时的 pre-flight 估算 / 截断」。

**Claude Code 对照.** 运行期不依赖本地重型 tokenizer 做预算;以服务端 usage 为准。

**验收.** `cargo build -p deepstrike-wasm`(默认 features)产物不含 BPE 表,体积下降;`--features tiktoken` 时计数行为与现状一致。

**风险.** 低。纯依赖隔离,默认行为(char_approx)不变。

---

### P1-6. 补齐 Phase D 唯一缺口:异步 LLM 语义摘要

**问题.** 截断/分级/保留最近 N 轮都已实现,但 Collapse/Auto 的摘要仍是 `RuleSummarizer`(只有条数/token/工具名/最后 200 字),语义损失大。

**现状.** 摘要器接口已就绪 [`summarizer.rs`](../crates/deepstrike-core/src/context/summarizer.rs);pipeline 写死 `RuleSummarizer`([`compression.rs:443`](../crates/deepstrike-core/src/context/compression.rs#L443));`CompressResult.summary` 与 `Compressed` observation 已贯通到 SDK,wake 时已注入([`runner.ts:874`](../node/src/runtime/runner.ts#L874))。

**方案.** 同步规则摘要先顶上(不阻塞热路径),异步 LLM 摘要回写——与 v2 spec §10 决策 #1 一致。
- Kernel 侧:`Compressed` observation 已带 `summary` + `archived`;新增「摘要待升级」标记或让 SDK 凭 `archived` 自行决定是否升级。
- SDK 侧:收到 `Compressed` 后,后台对 `archived` 调一次小模型(结构化 prompt:做了什么 / 改了哪些文件 / 当前状态 / 下一步),把结果作为「二次 `Compressed` 事件」回写 SessionLog,替换原 summary;wake 时优先用升级版。
- 结构化摘要模板参考 Claude Code 的 compact:`<analysis>` + `<summary>`(目标、已完成、关键决策、待办、相关文件)。

**验收.** AutoCompact 后,后台任务产出的 summary 含具体语义(目标/进度/文件),而非仅统计;wake 注入的是升级版;LLM 调用失败时回退规则摘要,不阻断主循环。

**风险.** 中。异步回写与 SessionLog 顺序、wake 读取需小心;务必非阻塞。

---

### P1-7. 控制性文本降噪 + opt-in

**问题.** rollback / dashboard 等控制叙述默认进 prompt,增加噪声、可能诱导模型变拘谨或多绕几轮。

**现状.**
- milestone / sub-agent **已是 opt-in**(靠 `LoadMilestoneContract` / `SpawnSubAgent`,默认不触发)——无需改。
- 默认开启且每次注入的是:rollback note `[SYSTEM] Transaction rollback: ...`([`state_machine.rs:362-368`](../crates/deepstrike-core/src/scheduler/state_machine.rs#L362-L368)、超时 [`:471-476`](../crates/deepstrike-core/src/scheduler/state_machine.rs#L471-L476))与 dashboard 块([`renderer.rs:27`](../crates/deepstrike-core/src/context/renderer.rs#L27))。

**方案.**
- rollback note:精简为简短自然语言(去掉 `[SYSTEM] Transaction rollback:` 这类内部术语),只告诉模型「上一步因 X 失败,请换方法」。
- dashboard:默认不渲染(归入 P0-2 的 `system_volatile`,且仅在显式 agent-os 模式开启);普通 SDK 路径只渲染 task_state 的 goal/plan/progress 必要项。
- 增加 `ContextConfig` 开关(沿用 v2 的 ratio-only 风格之外的布尔开关分区):`render_dashboard: bool`(默认 false)、`verbose_control_notes: bool`(默认 false)。

**验收.** 默认配置下渲染输出不含 `[SYSTEM] Transaction rollback`、不含 dashboard 块;开启 agent-os 模式后恢复。

**风险.** 低。

---

## P2 — 打磨(LLM 延迟为主,收益有限,最后做)

### P2-8. 减少 FFI / JSON 开销
- **archived 大 payload 过 FFI**:AutoCompact 时 `Compressed { archived: Vec<Message> }` 全量序列化过边界([`kernel.rs:205-210`](../crates/deepstrike-core/src/runtime/kernel.rs#L205-L210))。改为 kernel 内部暂存 + 返回 `archive_handle`,SDK 按需 `drainArchived(handle)` 拉取(仅写归档时才需要)。
- **per-step JSON 编解码**:每步 `JSON.stringify/parse`([`kernel-step.ts:340-364`](../node/src/runtime/kernel-step.ts#L340-L364))。相对 LLM 网络延迟可忽略,**除非** profiling 显示热点,否则不动;真要优化,走 WASM 共享内存 / 二进制 ABI,属大改,不在本期。

**风险.** archived handle 改动触及 ABI,需版本协商;**确认是瓶颈再做**。

---

## 实施顺序与里程碑

```
里程碑 1(质量+成本,1 PR/项,独立可验证)
  P0-1  token 计数修复        ← 先量「压缩触发次数」基线,改后对比
  P0-2  prompt caching         ← 先量「延迟/计费 token」基线,改后对比(只做 Anthropic)
  P0-3  renderer 最近优先 + P0-4 去重 goal   ← 纯 kernel,一起 1 个 PR,带测试

里程碑 2(瘦身+语义)
  P1-5  tiktoken feature 化     ← WASM 瘦身,独立 PR
  P1-6  异步 LLM 摘要           ← Phase D 收尾
  P1-7  控制文本降噪

里程碑 3(按需)
  P2-8  仅在 profiling 证明是瓶颈时
```

**最小高价值组合 = P0-1 + P0-2。** 这两项直接对应「变差(过早压缩)」与「变慢(无缓存)」,且互相独立、风险可控。建议先各自落地并用基线数据验证收益,再推进其余。

---

## 测试计划

### 单元
| 测试 | 文件 | 断言 |
|---|---|---|
| `rho_grows_linearly_not_quadratic` | `pressure_tests.rs` | 10 轮固定增量,rho 近似线性 |
| `observed_tokens_override_estimate` | `pressure_tests.rs` | 有 `observed_input_tokens` 时 rho 用观测值 |
| `assistant_msg_token_is_output_only` | provider 单测(node) | 消息 `tokenCount` ≤ `output_tokens` |
| `render_keeps_latest_user_turn` | `renderer_tests.rs` | 超预算时最新 user turn 必留且未截 |
| `render_truncates_oldest_not_newest` | `renderer_tests.rs` | 被截的是最旧消息 |
| `system_stable_is_byte_stable` | `renderer_tests.rs` | 一次 run 内 `system_stable` 哈希不变 |
| `goal_rendered_once` | `state_machine_tests.rs` | goal 不在 user message 重复 |
| `wasm_build_excludes_tiktoken` | CI 脚本 | 默认 features 产物不含 BPE 表 |

### 集成
| 场景 | 期望 |
|---|---|
| 长会话修复前后对比 | `Compressed` observation 次数显著下降(P0-1) |
| 多轮 Anthropic 调用 | 第 2 轮起 `cache_read_input_tokens > 0`(P0-2) |
| AutoCompact + 异步 LLM 摘要 | 后台产出语义摘要并回写,wake 用升级版(P1-6) |
| 非 Anthropic provider | `system_stable`/`system_volatile` 拼回,行为不变(P0-2 兼容) |

---

## 兼容性

| 改动 | 兼容性 | 说明 |
|---|---|---|
| `ProviderResult` 增 `observed_*_tokens` | 加法,可选字段 | 缺省回退逐条求和 |
| `RenderedContext` 拆 `system_stable/volatile` | 破坏性(内部类型)+ SDK 适配 | 不支持缓存的 provider 拼回 `system_stable+volatile` |
| `cache_control` | 仅 Anthropic 启用 | 其它 provider 不变 |
| renderer 顺序 | 行为变更 | 仅影响超预算边界场景,正常路径不变 |
| `deepstrike-tokenizer` 转 optional | 加法(feature) | 默认行为 = char_approx,与现状默认一致 |
| 异步 LLM 摘要 | 加法,opt-in | 失败回退 RuleSummarizer |
| `render_dashboard` / `verbose_control_notes` 开关 | 加法,默认更安静 | agent-os 模式恢复旧行为 |

**建议版本:** `0.3.0`(`RenderedContext` 拆分与 renderer 顺序属行为变更)。

---

## 待决策

| # | 问题 | 倾向 |
|---|---|---|
| 1 | rho 完全以 provider usage 为准,还是 usage 与估算取 max? | **以 usage 为准**;无 usage 才估算。取 max 会保留高估问题 |
| 2 | `system_volatile` 放「最后 turn 后的 system 消息」还是「并入最后 user turn」? | **独立 system-role 消息**(贴近 Claude Code system-reminder,且不破坏 user/assistant 交替) |
| 3 | 缓存断点是否也覆盖历史前缀(增量缓存)? | 先只缓存 system+tools;历史增量缓存作为 P0-2 的后续增强 |
| 4 | 异步 LLM 摘要用哪个模型? | SDK 可配,默认小模型(haiku 级),控制成本 |
| 5 | P2-8 是否本期做? | 否,除非 profiling 证明 FFI 是瓶颈 |
