# DeepStrike Examples

跨平台 SDK 示例。所有平台共用同一个真实场景——**FlashNote 闪念整理助手**——用同一套剧本，在 `node` / `python` / `rust` / `wasm` 四端各实现一遍，直观展示跨语言 API 的一致性与差异。

选场景的标准：**`tests/` 里 10 个模块在场景里都能找到非演示性的真实出场理由**，不存在硬塞。

---

## 场景：FlashNote —— 闪念整理助手

### 核心模式：知识飞轮

```text
社区活动 / 个人闪念
        ↓
   AI 主动采集 + 结构化
        ↓
   策展 · 去重 · 关联
        ↓
   数据资产（个人知识库 / 训练语料 / RAG 语料库）
        ↓
   更好的产品 → 更多贡献者
```

FlashNote 既是个人闪念整理工具，也是社区知识采集平台。两条路径共用同一套 ingest → harness → archive pipeline，区别在于输入来源和输出格式：

- **个人模式**：随手扔进来——想法片段、文章 URL、本地文件——agent 自动分类、打标签、找关联，随时 `/export` 拿摘要或聚类。
- **社区模式**：活动现场或异步收集，agent 主动提问提取结构化洞察，多人贡献汇入同一 archive，输出高质量训练语料或 RAG 语料库。
- **深研模式**：`/research <topic>` 触发，agent 切换到深度调研，产出流入同一 archive，与闪念共用质检流程。

### 10 个模块的出场点

| # | 模块 | 在 FlashNote 里的角色 |
| --- | --- | --- |
| 01 | provider | `OpenAIProvider` + `CircuitBreaker`：常驻守护进程，上游抖动必须熔断自愈 |
| 02 | agent | 个人/社区捕获 `maxTurns=5`；`/research` 切 `maxTurns=20`；`pressure` 检测队列积压 |
| 03 | tools | `capture_text` / `ingest_file` / `fetch_and_clip` / `search_archive` / `export` / `deep_research` / `interview_capture` / `export_dataset` |
| 04 | skills | `classify_and_tag` / `find_connections` / `synthesize_cluster` / `write_digest` / `outline_research` / `summarize_source` / `elicit_insight` |
| 05 | memory | `WorkingMemory` 跟踪当批上下文；`DreamStore` 沉淀跨 session 的主题偏好与社区知识图谱 |
| 06 | knowledge | archive 同时作为个人知识库和社区语料库，检索时按来源权重区分 |
| 07 | harness | 三套 criteria：个人笔记 / 社区贡献（去重 + 归因 + 独特性）/ 研究报告 |
| 08 | signals | `scan_inbox` 定时扫文件夹；stdin 桥 `/dump` `/research` `/interview` `/export` `/export-dataset` `/stop` |
| 09 | governance | `export_dataset` / `deep_research` 写盘 → `ask_user`；PII 检测 → `deny`；贡献者归因追踪 |
| 10 | combos | 整个示例本身就是 combos 的现实形态 |

---

## 目录布局

```text
example/
├── node/         # 完整实现（当前进行中）
├── python/       # node 跑通后镜像
├── rust/         # node 跑通后镜像
├── wasm/         # node 跑通后镜像（浏览器 / Workers / Deno）
└── integration/  # 跨平台联合 demo
```

## 实现进度

### node

- [ ] 骨架 + provider + agent + main（`/dump` 一条记录并打印处理结果）
- [ ] 基础 tools + governance（export ask_user / fetch deny）
- [ ] 个人模式 skills + `note_judge`
- [ ] archive_source + working memory
- [ ] inbox_watcher + cli_bridge
- [ ] `deep_research` + 研究 skills + `report_judge`
- [ ] `interview_capture` + `elicit_insight` skill（社区采集模式）
- [ ] `export_dataset` + PII 过滤 + 贡献者归因（数据产品输出）
- [x] DreamStore + `RuntimeRunner.dream()` (Node / Python / Rust / WASM)

### 其他平台与 Integration

- [ ] python / rust / wasm 镜像
- [ ] 跨平台联合 demo

## 运行前置

所有平台共用根目录 `.env`：

```env
OPENAI_API_KEY=...
OPENAI_BASE_URL=...
OPENAI_MODEL=...
```

深研模式和社区采集可选：

```env
SERPAPI_API_KEY=...     # 或 TAVILY_API_KEY，deep_research 外网检索
JINA_API_KEY=...        # 可选，jina.ai reader 增强网页正文提取
```
