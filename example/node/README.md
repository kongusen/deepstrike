# FlashNote —— Node SDK 示例

`@deepstrike/sdk` 在一个持续运转的知识飞轮场景里串起 10 个模块的完整示范。

FlashNote 是**常驻守护进程**，同时支持三条输入路径，产出汇入同一个 archive，共用同一套 ingest → harness → archive pipeline：

| 路径 | 触发方式 | 典型场景 |
| --- | --- | --- |
| **个人闪念** | `/dump`、inbox 文件夹、URL | 随手记录想法、剪藏文章 |
| **社区采集** | `/interview`、批量异步提交 | 活动现场主动采访、会后问卷 |
| **深度研究** | `/research <topic>` | 对某个主题做完整调研 |

## 模块映射

每个文件直接对应 `tests/node/0X_*.test.ts` 中验证过的能力。

| 测试 | SDK 能力 | 本示例文件 | 在场景里做什么 |
| --- | --- | --- | --- |
| `01_provider` | `OpenAIProvider`、`CircuitBreaker` | `src/provider.ts` | 构造 provider，包熔断器；常驻进程中上游连续 5 次失败即熔断 60s 自愈 |
| `02_agent` | `Agent.run`、`pressure` | `src/agent.ts` | 个人/社区捕获 `maxTurns=5` 短回路；`/research` 后切 `maxTurns=20`；`pressure>0.7` 触发批量 flush |
| `03_tools` | `tool()`、`executeTools()` | `src/tools/*.ts` | 注册 8 个工具（详见下表） |
| `04_skills` | `scanSkillDir()`、`skillDir` | `skills/*.md` | LLM 在不同阶段自动挑套路，不硬编码 prompt |
| `05_memory` | `WorkingMemory`、`DreamStore`、`Agent.dream()` | `src/memory/{working,dream_store}.ts` | 当批上下文用 WorkingMemory；session 结束调 `dream()` 沉淀主题图谱，下次召回 |
| `06_knowledge` | `KnowledgeSource.retrieve()` | `src/knowledge/archive_source.ts` | archive 同时作为个人知识库和社区语料库，检索时按来源权重区分 |
| `07_harness` | `HarnessLoop`（LLM-as-judge） | `src/harness/{note,contribution,report}_judge.ts` | 三套 criteria（见下），未达标重处理 ≤2 次 |
| `08_signals` | `SignalGateway`、`ScheduledPrompt` | `src/signals/{inbox_watcher,cli_bridge}.ts` | 定时扫 inbox + stdin 接 `/dump` `/interview` `/research` `/export` `/export-dataset` `/stop` |
| `09_governance` | `Governance`（allow/deny/ask_user） | `src/governance/policy.ts` | `export_dataset` / `deep_research` 写盘 → `ask_user`；PII 命中 → `deny`；贡献者归因追踪 |
| `10_combos` | 上述组合 | 整个 `src/main.ts` 入口 | — |

## 工具一览

| 工具 | 参数 | 行为 | governance |
| --- | --- | --- | --- |
| `capture_text` | `{ text }` | 接收一段文字，进入处理队列 | `allow` |
| `ingest_file` | `{ path }` | 读取 md / txt / pdf / docx，提取正文 | `allow`（路径限 `inbox/`，否则 `deny`） |
| `fetch_and_clip` | `{ url }` | 抓网页正文截断到 6 KB；jina reader 可选 | 已知干扰域名 `deny`，其他 `allow` |
| `search_archive` | `{ query, topK, source? }` | 在 archive 中全文检索；`source` 可筛 personal / community | `allow` |
| `export` | `{ format, filter? }` | 生成 digest / outline / actions / clusters | `ask_user`（写文件） |
| `deep_research` | `{ topic, maxSources? }` | 切深研模式：web_search → fetch_and_clip → 笔记 → report_judge → 入库 | `ask_user`（外网 + 写盘） |
| `interview_capture` | `{ contributor, topic, mode }` | agent 主动提问，逐轮提取结构化洞察；`mode: live\|async` | `allow`（记录归因元数据） |
| `export_dataset` | `{ format, quality_threshold, anonymize? }` | 按质量阈值过滤 archive，输出 JSONL / RAG 语料 / memory pack | `ask_user`（写盘）+ PII 自动 `deny` |

## Skills 一览

三条路径共用 skill 目录，LLM 按阶段自动选用。

| Skill | when to use | effort |
| --- | --- | --- |
| `classify_and_tag` | 每条新输入，确定类别和标签 | low |
| `find_connections` | 有 archive 命中时，找关联并注释 | medium |
| `synthesize_cluster` | 一批相关笔记凑够 3 条时，融合成洞察 | high |
| `write_digest` | 收到 `/export digest` 时，生成今日摘要 | medium |
| `elicit_insight` | `interview_capture` 进行中，设计下一个追问 | medium |
| `outline_research` | `deep_research` 启动时，先做提纲规划 | low |
| `summarize_source` | 抓完一篇文章准备落笔记时 | medium |

## HarnessLoop criteria

### 个人笔记（`note_judge`）

1. 至少 2 个标签（`#tag` 格式）
2. 一行不超过 80 字的摘要
3. 至少一条关联笔记 ID，或显式标记 `#island`
4. 不得是已有笔记的逐字重复

### 社区贡献（`contribution_judge`）

在个人笔记 criteria 基础上额外检查：

1. 包含贡献者归因字段（`contributor_id`，可匿名化）
2. 洞察具体可引用：有具体案例、数据或判断，拒绝泛泛而谈
3. 与 archive 中已有社区贡献的相似度 < 0.85（去重）

### 研究报告（`report_judge`）

1. 引用 ≥ 3 个独立来源，每条论点有可点击 URL
2. 字数 600–1200
3. 结构含：TL;DR / 对比表或要点列表 / 结论 / 引用

## 信号设计

- `ScheduledPrompt("scan_inbox", +30s)`：每 30 秒扫一次 `inbox/`，新文件推入处理队列
- stdin 桥：
  - `/dump <text>` → 个人闪念，等价于往 inbox 扔临时文件
  - `/interview [contributor] [topic]` → 启动主动采集，agent 开始逐轮追问
  - `/research <topic>` → 切深研模式（`maxTurns=20`），产出流入 archive
  - `/export [digest|outline|actions|clusters]` → 生成可读输出，默认 `digest`
  - `/export-dataset [jsonl|rag|memory_pack] [--min-quality 0.8] [--anonymize]` → 生成数据产品
  - `/cluster <topic>` → 对某主题做聚类融合
  - `/stop` → 干净终止，调 `agent.dream()` 沉淀后退出
- `agent.pressure > 0.7` → 外层主动触发 flush，批量处理积压队列

## 目录骨架

```text
example/node/
├── README.md
├── package.json
├── tsconfig.json
├── .env.example
├── skills/
│   ├── classify_and_tag.md
│   ├── find_connections.md
│   ├── synthesize_cluster.md
│   ├── write_digest.md
│   ├── elicit_insight.md         # interview_capture 追问设计
│   ├── outline_research.md
│   └── summarize_source.md
├── src/
│   ├── main.ts                   # 守护进程入口
│   ├── provider.ts               # 01
│   ├── agent.ts                  # 02 支持动态切换 maxTurns
│   ├── tools/                    # 03
│   │   ├── capture_text.ts
│   │   ├── ingest_file.ts
│   │   ├── fetch_and_clip.ts
│   │   ├── search_archive.ts
│   │   ├── export.ts
│   │   ├── deep_research.ts
│   │   ├── interview_capture.ts  # 主动采集，记录归因元数据
│   │   └── export_dataset.ts     # 数据产品输出，含 PII 过滤
│   ├── memory/                   # 05
│   │   ├── working.ts
│   │   └── dream_store.ts        # 沉淀主题图谱 + 社区高频洞察模式
│   ├── knowledge/                # 06
│   │   └── archive_source.ts     # 个人 + 社区双权重检索
│   ├── governance/               # 09
│   │   └── policy.ts             # export_dataset PII deny + 贡献者归因 allow
│   ├── signals/                  # 08
│   │   ├── inbox_watcher.ts
│   │   └── cli_bridge.ts         # 解析 /dump /interview /research /export /export-dataset /stop
│   └── harness/                  # 07
│       ├── note_judge.ts
│       ├── contribution_judge.ts # 社区贡献去重 + 独特性检查
│       └── report_judge.ts
├── inbox/                        # 扔文件进来，自动处理
├── archive/                      # 结构化笔记（json + md，含归因元数据）
└── output/
    ├── digest/
    ├── clusters/
    └── datasets/                 # export_dataset 输出（JSONL / RAG / memory pack）
```

## 运行

```bash
cd example/node
npm install
cp ../../.env .env

npm run dev
```

启动后三种使用方式：

```bash
# 个人闪念
/dump MoE 架构里专家路由稀疏激活的核心是 load balancing loss，不是 routing 本身

# 社区采集（活动现场）
/interview alice "AI创业中的冷启动策略"
# agent 开始追问，alice 逐轮回答，结束后自动入库

# 深度研究
/research 2026 主流开源 LLM 推理引擎对比

# 数据产品输出
/export-dataset jsonl --min-quality 0.8 --anonymize
```

重启后，`DreamStore` 里的历史主题图谱和社区高频洞察模式会被召回，agent 能识别常被讨论的领域，自动提升相关笔记的关联优先级。

## 实现顺序（增量交付）

每一步都能独立跑、独立看效果。

1. 骨架 + `provider.ts` + `agent.ts` + `main.ts`：能接受 `/dump` 并打印分类结果
2. 5 个基础 tools + `governance/policy.ts`：演示 `export` ask_user、干扰域名 deny
3. 个人模式 skills + `harness/note_judge.ts`：看到不合格笔记被重处理
4. `archive_source.ts` + `memory/working.ts`：入库前检索，看到去重和关联提示
5. `inbox_watcher.ts` + `cli_bridge.ts`：文件夹监听上线，stdin 命令全部可用
6. `deep_research` tool + 研究 skills + `harness/report_judge.ts`：`/research` 命令跑通
7. `interview_capture` tool + `elicit_insight` skill + `harness/contribution_judge.ts`：`/interview` 跑通，看到 agent 追问
8. `export_dataset` tool + PII 过滤 + 归因元数据：`/export-dataset` 输出可用 JSONL
9. `memory/dream_store.ts` + `agent.dream()`：重启后验证主题图谱被召回
