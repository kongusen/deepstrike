# QuantStrike

基于 [DeepStrike](https://github.com/kongusen/deepstrike) Agent 运行时与 [AKShare](https://github.com/akfamily/akshare) 的 A 股量化研究与交易系统设计文档。

> **状态**：设计阶段（尚未实现代码骨架）

---

## 1. 项目定位

QuantStrike 是一个**研究优先、逐级升级**的量化平台：

| 阶段 | 能力 | Agent 参与度 |
|------|------|-------------|
| 研究 | 行情扫描、因子筛选、报告生成 | 高 |
| **ML 预测** | 特征工程、训练、验证、推理、信号接入 | 中（实验编排 + 解读，**不训练**） |
| 回测 | 声明式策略 spec、绩效分析 | 中（编排 + 解读） |
| 模拟盘 | 虚拟下单、持仓跟踪 | 低（信号触发 + 审批） |
| 实盘 | 真实下单（可选） | 极低（仅风控审批链） |

**核心原则**：与 DeepStrike 一致——**Rust 内核做决策，Python SDK 做 I/O**。AKShare 仅提供行情与基本面数据，**不提供下单接口**；指标计算、回测、**订单路由**全部封装在确定性 Python 模块与 Tool 层；LLM 负责编排、解释与风控对话，**不直接调用券商 API**。

### 1.1 设计支柱

| 支柱 | 做法 |
|------|------|
| **LLM 防爆盾** | LLM 不训练、不推理、不调券商；PreTrade / 特征计算 / 回测 / 训练全部确定性执行；禁止 `exec()` 跑 Agent 生成的代码 |
| **PIT + Embargo** | 基本面以 `announce_date` 对齐；标签与特征间强制禁运期；切断 Agent 系统中最常见的前视偏差 |
| **声明式 Spec** | 策略、模型、订单意图一律 JSON spec；可解析、可校验、可审计，契合 DeepStrike 结构化 Tool 原语 |

---

## 2. 总体架构

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Layer 4 · 应用层  QuantStrike App                                       │
│  CLI / Web Dashboard / Webhook 告警 / 定时任务调度                         │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────────────────┐
│  Layer 3 · 协作层  多 Agent 工作流                                        │
│  ResearchOrchestrator → StrategyExecutor → RiskVerifier                   │
│  (CreatorVerifierMode / VerificationContract)                           │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────────────────┐
│  Layer 2 · 协作原语                                                       │
│  VerificationContract · AgentPool · HandoffBus · TaskLane               │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │
┌───────────────────────────────▼─────────────────────────────────────────┐
│  Python SDK  RuntimeRunner + LocalExecutionPlane                         │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐      │
│  │ Data     │ │ Analysis │ │ ML       │ │ Backtest │ │ Execution│      │
│  │ (AKShare)│ │(pandas)  │ │ Pipeline │ │ Engine   │ │(Paper/   │      │
│  │          │ │          │ │(sklearn/ │ │          │ │ Live)    │      │
│  │          │ │          │ │ lightgbm)│ │          │ │          │      │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘      │
│       └────────────┴────────────┴────────────┴────────────┘             │
│  Skills · DreamStore · KnowledgeSource · GovernancePipeline             │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ FFI
┌───────────────────────────────▼─────────────────────────────────────────┐
│  deepstrike-core  LoopStateMachine / ContextEngine / SignalRouter        │
└─────────────────────────────────────────────────────────────────────────┘

         ┌──────────────────────────────────────────┐
         │  确定性子系统（非 Agent，纯 Python）       │
         │  行情缓存 · 特征库 · ML 训练/推理 · 回测   │
         │  · 模型注册表 · 订单路由                   │
         └──────────────────────────────────────────┘
```

### 与 DeepStrike 能力映射

| DeepStrike 能力 | QuantStrike 用法 |
|----------------|-----------------|
| `RuntimeRunner` | 研究、复盘、报告生成主循环 |
| `LocalExecutionPlane` | 注册 AKShare 与分析 Tools |
| Skills | 标准化投研流程（扫描、回测、风控） |
| `DreamStore` | 工程教训 + Harness 统计结论（限域，见 §9.2） |
| `KnowledgeSource` | 策略文档、交易规则 RAG |
| `GovernancePipeline` | 实盘审批、限流、标的黑名单 |
| `SignalRouter` | 盘前 cron、价格告警、用户中断 |
| Harness + EvalPipeline | 策略报告 + **ML 实验报告**质量评分与重试 |
| `AgentPool` + Contract | 研究 / ML 执行 / 审计三角色隔离 |

---

## 3. 目录结构（规划）

```
example/python/quantstrike/
├── README.md                   # 本文档
├── main.py                     # CLI 入口
├── runtime.py                  # RuntimeRunner 组装
├── governance/
│   └── policy.py               # 下单 ask_user、限流、黑名单
├── tools/
│   ├── data/                   # AKShare 封装
│   │   ├── quote.py            # 实时 / 历史行情
│   │   ├── fundamental.py      # 财报、估值
│   │   ├── market.py           # 板块、资金流、北向
│   │   └── macro.py            # 宏观指标
│   ├── analysis/
│   │   ├── indicators.py       # MA / MACD / RSI
│   │   ├── screener.py         # 选股筛选
│   │   └── factor.py           # 因子计算
│   ├── ml/                     # 机器学习 Tools（见 §5）
│   │   ├── features.py         # 从 feature_store 筛选（只读）
│   │   ├── labels.py           # 标签构造
│   │   ├── train.py            # 训练与交叉验证
│   │   ├── evaluate.py         # IC / OOS / 费后 IC 评估
│   │   ├── predict.py          # 批量推理
│   │   └── registry.py         # 模型版本管理
│   ├── audit/
│   │   └── verify_artifact.py  # 产物完整性校验（见 §10.1）
│   ├── backtest/
│   │   ├── run.py              # 回测执行
│   │   └── report.py           # 绩效报告
│   └── execution/
│       ├── propose.py          # 生成 OrderIntent（见 §6）
│       ├── paper.py            # 模拟盘 Tool 入口
│       └── live.py             # 实盘 Tool 入口
├── execution/                  # 订单核心逻辑（非 Tool，纯函数，见 §6）
│   ├── intent.py               # OrderIntent 定义
│   ├── pre_trade.py            # A 股 PreTrade 风控（T+1、涨跌停…）
│   ├── pending_pool.py         # 本地资金 / 持仓冻结池（见 §6.6）
│   ├── router.py               # OrderRouter 统一入口
│   ├── paper_adapter.py        # 模拟撮合
│   ├── sync.py                 # 与券商持仓对账
│   └── live/
│       ├── base.py             # ExecutionAdapter 协议
│       └── qmt.py              # mini QMT / xtquant（可换 vnpy）
├── ml/                         # ML 核心逻辑（非 Tool，纯函数）
│   ├── feature_store.py        # 特征定义与 point-in-time 对齐
│   ├── label_builder.py        # 前瞻收益 / 分类标签
│   ├── validators.py           # 走步验证、Purged K-Fold
│   ├── models/                 # 模型实现
│   │   ├── lightgbm_ranker.py
│   │   ├── linear_factor.py
│   │   └── ensemble.py
│   └── metrics.py              # IC、Rank IC、IR、校准曲线
├── store/
│   ├── parquet_cache.py        # 行情本地缓存
│   ├── feature_store/          # 特征 Parquet（按 date × symbol）
│   ├── models/                 # 模型 artifact + metadata.json
│   ├── experiments/            # 实验记录（params、metrics、fold）
│   ├── predictions/            # 每日推理结果
│   ├── portfolio.py            # 持仓 / 订单持久化
│   └── backtest_results.py
├── knowledge/
│   └── strategy_docs.py        # 策略文档 RAG
├── memory/
│   └── dream_store.py          # 跨会话交易经验
├── jobs/                       # 确定性后台任务（非 Agent 触发）
│   ├── feature_pipeline.py     # 收盘后全量特征计算（cron 15:30）
│   └── broker_sync.py          # 券商状态轮询对账
├── signals/
│   ├── market_calendar.py      # 交易日 cron
│   └── price_alert.py          # 价格 / webhook 信号
├── harness/
│   └── strategy_judge.py       # LLM-as-judge 评策略报告
└── skills/
    ├── daily_market_scan.md
    ├── factor_screening.md
    ├── backtest_strategy.md
    ├── ml_feature_engineering.md
    ├── ml_train_evaluate.md
    ├── ml_signal_integration.md
    ├── risk_review.md
    └── post_trade_review.md
```

数据落盘（运行时生成）：

```text
store/
  cache/          *.parquet     行情缓存
  feature_store/  *.parquet     特征矩阵（point-in-time）
  models/         */            模型版本（model.pkl + metadata.json）
  experiments/    *.json        ML 实验记录
  predictions/    *.parquet     每日推理分数
  portfolio/      *.json        现金、持仓、成本（模拟盘 / 实盘镜像）
  orders/         *.json        订单与成交记录（intent → fill 审计链）
  backtest/       *.json        回测结果（含 content_hash，Agent 无写权限）
  artifacts/      */            确定性产物 + manifest（见 §10.1）
output/
  sessions/                     SessionLog
  memory/quantstrike/
    memories.json                 DreamStore 沉淀
  reports/        *.md          研究报告导出
```

---

## 4. 工具层设计

每个 Tool 注册为 DeepStrike `RegisteredTool`，输入 / 输出为 JSON，便于内核审计与治理。

### 4.1 数据工具（只读，默认 allow）

| Tool | 说明 | AKShare 参考 |
|------|------|-------------|
| `fetch_daily_bars` | 日 K 线（前复权） | `stock_zh_a_hist` |
| `fetch_realtime_quotes` | 实时行情快照 | `stock_zh_a_spot_em` |
| `fetch_financial_indicator` | 财务指标 | `stock_financial_analysis_indicator` |
| `fetch_sector_flow` | 板块资金流 | `stock_sector_fund_flow_rank` |
| `fetch_northbound_flow` | 北向资金 | `stock_hsgt_north_net_flow_in_em` |
| `fetch_index_constituents` | 指数成分股 | `index_stock_cons` |

**实现约定**：

- 所有 AKShare 调用经 `store/parquet_cache.py` 统一缓存，键为 `(symbol, freq, date_range)`
- Tool 返回**结构化摘要**（最新价、涨跌幅、关键指标），不直接把大 DataFrame 塞给 LLM
- 全量数据写 Parquet，Tool 返回 `cache_key` + 统计摘要

### 4.2 分析工具（只读）

| Tool | 说明 |
|------|------|
| `compute_indicators` | 基于 cache_key 计算 MA / MACD / RSI 等 |
| `screen_stocks` | 按条件筛选 universe，返回 top-N |
| `rank_by_factor` | 因子排序（PE、ROE、动量等） |
| `correlate` | 多标的收益率相关性 |

计算逻辑在 Python 纯函数中完成，Agent 只解读结果。

### 4.3 回测工具（写结果，rate_limit）

| Tool | 说明 |
|------|------|
| `run_backtest` | 按 strategy_spec 执行回测 |
| `get_backtest_report` | 读取回测绩效 |
| `compare_backtests` | 多策略对比 |

策略使用**声明式 JSON spec**（禁止 LLM 生成可执行 Python）：

```json
{
  "name": "ma_cross",
  "universe": "hs300",
  "entry": { "indicator": "ma_cross", "fast": 5, "slow": 20 },
  "exit": { "stop_loss": 0.08, "take_profit": 0.15 },
  "position_size": 0.1,
  "max_positions": 5
}
```

### 4.4 执行工具（强治理，详见 §6）

| Tool | 说明 | 治理级别 |
|------|------|----------|
| `get_portfolio` | 查询现金、持仓、成本 | allow |
| `get_open_orders` | 查询未完成委托 | allow |
| `propose_order` | 生成并校验 OrderIntent，返回「建议单」 | allow |
| `propose_rebalance` | 根据目标权重批量生成 intent 列表 | allow |
| `place_paper_order` | 模拟盘落账 + 模拟撮合 | allow + rate_limit |
| `place_live_order` | 真实委托（经 OrderRouter → 券商） | **ask_user** + veto |
| `cancel_order` | 撤单 | **ask_user**（实盘） |
| `rebalance` | 按目标权重批量调仓 | **ask_user**（实盘） |
| `sync_broker_state` | 与券商对齐持仓 / 订单 | allow |

**原则**：Agent 默认只调 `propose_*`；`place_live_order` 建议由 UI / cron 在用户批准后触发，避免 LLM 误触实盘。

### 4.5 ML 工具

| Tool | 说明 | 治理 |
|------|------|------|
| `query_feature_matrix` | 从已构建的 `feature_store` **筛选** universe + 特征列（只读） | allow |
| `build_labels` | 构造前瞻收益 / 涨跌分类标签 | allow |
| `train_model` | 按 `model_spec` 走步训练 | rate_limit |
| `evaluate_model` | OOS 指标：IC、Rank IC、AUC、校准 | allow |
| `predict` | 对 universe 批量推理，写 predictions | allow |
| `list_models` / `get_model` | 模型注册表查询 | allow |
| `deploy_model` | 标记 production 版本 | **ask_user** |
| `run_ml_backtest` | 用模型预测分数驱动回测 | rate_limit |

**原则**：训练、推理、矩阵运算在 `ml/` 纯 Python 模块内完成；Agent 只提交声明式 spec 并解读评估报告。**全量特征计算由 cron 后台 job 完成，Agent 不得按需触发全 universe 滚动计算**（见 §5.3.1）。

---

## 5. 机器学习预测子系统

ML 层负责**预测未来收益 / 排名 / 概率**，输出标准化信号供回测与选股消费。与回测引擎一样，属于确定性计算；DeepStrike Agent 负责实验设计、参数选择讨论、结果解释与是否上线决策。

### 5.1 在整体链路中的位置

```
AKShare 原始数据
      │
      ▼
特征工程 (feature_store) ──► 标签 (label_builder)
      │                           │
      └───────────┬───────────────┘
                  ▼
           训练 / 走步验证 (validators)
                  │
                  ▼
           模型注册表 (store/models/)
                  │
      ┌───────────┴───────────┐
      ▼                       ▼
  predict (推理)         run_ml_backtest
      │                       │
      ▼                       ▼
  选股 / 排名信号           策略绩效报告
      │
      ▼
  strategy_spec.signal = { "type": "ml_score", "model_id": "..." }
      │
      ▼
  模拟盘 / 实盘（治理审批）
```

### 5.2 预测任务定义

| 任务类型 | 标签 | 典型模型 | 下游用法 |
|----------|------|----------|----------|
| **横截面排名** | 未来 N 日收益率 | LightGBM Ranker / LambdaRank | 每日 top-K 多头 |
| **二元分类** | 未来 N 日收益 > 0（或 > 基准） | LightGBM / Logistic | 过滤 + 仓位加权 |
| **回归** | 未来 N 日对数收益 | Ridge / ElasticNet | 线性组合权重 |
| **波动预测** | 未来 N 日实现波动 | GARCH / 简单 ML | 风控、仓位缩放 |

默认先从**横截面排名**（Alpha 因子预测）起步，与 A 股多标的选股场景最匹配。

### 5.3 特征体系

特征分三层，统一写入 `store/feature_store/`：

```
Layer 1 · 价量特征（来自 AKShare + indicators）
  收益率(1/5/20/60d)、波动率、换手率、Amihud 非流动性、MA 偏离度

Layer 2 · 基本面特征（财报 point-in-time）
  PE/PB/ROE/营收增速 — 必须使用「公告日」对齐，禁止用未来财报

Layer 3 · 市场微观 / 情绪（可选）
  北向净流入、板块资金流、融资余额变化
```

**Point-in-Time（PIT）约束**（防前视偏差）：

- 每个 `(date, symbol)` 行只包含该日**已公开**的信息
- 财报特征以 `announce_date` 为准 merge，不用 `report_period` 结束日
- 训练集与推理集共用同一套 `feature_store.py` 逻辑

#### 5.3.1 特征库：Cron 构建，Agent 只读

A 股 5000+ 标的 × 多期 rolling 因子 × PIT 财报合并，若由 Agent 按需触发会导致计算爆炸与 Parquet IO 雪崩。

```
cron 15:30（确定性 jobs/feature_pipeline.py，非 Agent）
    │
    ├─ 读 Parquet 行情缓存
    ├─ 全 universe 批量计算价量 / 基本面特征
    ├─ 写入 store/feature_store/{trade_date}.parquet
    └─ 更新 manifest（date、feature_hash、row_count）

Agent 调用 query_feature_matrix(universe, features, as_of)
    │
    └─ 仅从已构建分区中 Filter / Project，禁止触发 rolling 重算
```

| 角色 | 能否触发全量特征计算 |
|------|---------------------|
| `jobs/feature_pipeline.py` | 是（cron） |
| Agent / `query_feature_matrix` | 否（只读筛选） |
| `train_model` | 否（读已有 feature_store） |

### 5.4 标签构造

```json
{
  "label_type": "forward_return",
  "horizon_days": 5,
  "benchmark": "hs300",
  "target": "excess_return",
  "winsorize": [0.01, 0.99]
}
```

| 字段 | 说明 |
|------|------|
| `horizon_days` | 预测 horizon（A 股常用 5 / 10 / 20） |
| `benchmark` | 超额收益基准（可选） |
| `target` | `raw_return` / `excess_return` / `binary` |
| `winsorize` | 极端值截尾，防止标签噪声主导 |

标签与特征之间强制 **`embargo`**（禁运期 ≥ horizon），避免标签泄露到特征窗口。

### 5.5 模型 spec（声明式，禁止 LLM 写训练代码）

```json
{
  "name": "lgb_rank_5d_v1",
  "task": "rank",
  "universe": "hs300",
  "features": ["ret_5d", "ret_20d", "vol_20d", "turnover_ratio", "pe_ttm", "roe"],
  "label": { "label_type": "forward_return", "horizon_days": 5, "target": "excess_return" },
  "model": {
    "type": "lightgbm",
    "params": { "num_leaves": 31, "learning_rate": 0.05, "n_estimators": 200 }
  },
  "validation": {
    "method": "walk_forward",
    "train_window_days": 504,
    "test_window_days": 63,
    "step_days": 63,
    "embargo_days": 5
  },
  "metrics_threshold": {
    "mean_rank_ic": 0.03,
    "ic_ir": 0.5,
    "max_train_test_gap": 0.02
  }
}
```

### 5.6 验证方法论

```
时间轴 ─────────────────────────────────────────────►

Walk-Forward:
|── train ──|── test ──|
            |── train ──|── test ──|
                        |── train ──|── test ──|

Purged K-Fold（可选）:
  fold 边界处剔除 embargo 窗口，避免相邻 fold 标签重叠
```

**必报指标**（`evaluate_model` 输出摘要给 Agent，完整表写 `experiments/`）：

| 指标 | 含义 | 合格参考 |
|------|------|----------|
| Rank IC | 预测排名 vs 实现收益排名相关性 | mean > 0.03 |
| IC IR | IC 均值 / IC 标准差 | > 0.5 |
| **decay_ratio** | 测试 IC / 训练 IC（过拟合惩罚） | gap < 阈值（如 0.02） |
| **turnover_cost_adjusted_ic** | 扣除换手成本后的 IC | 与 Rank IC 同量级 |
| **net_ic_after_fees** | 含印花税 / 佣金后的净 IC | **Harness 硬门槛**，不过关则禁止 deploy 讨论 |
| Top-Bottom Spread | 分组多空收益差 | 稳定为正 |
| Turnover | 每日持仓变化 | 与成本匹配；日换 >40% 需告警 |

`evaluate_model` 输出 JSON 必须包含 `net_ic_after_fees` 与 `decay_ratio`。Harness 在 LLM-as-judge 之前做**确定性拦截**——费后 IC 不达标则直接 fail，不进入 Agent 「讨论进阶」环节。

### 5.7 推理与信号接入

每日收盘后（或盘前）执行：

```text
predict(model_id="lgb_rank_5d_v1", as_of="2026-05-22", universe="hs300")
  → store/predictions/2026-05-22.parquet
  → 返回 top_20 摘要 JSON 给 Agent
```

回测 / 实盘策略通过 `strategy_spec` 引用 ML 信号：

```json
{
  "name": "ml_top20_rebalance",
  "universe": "hs300",
  "signal": {
    "type": "ml_score",
    "model_id": "lgb_rank_5d_v1",
    "top_k": 20,
    "rebalance": "weekly"
  },
  "exit": { "stop_loss": 0.08 },
  "position_size": 0.05,
  "max_positions": 20
}
```

`run_ml_backtest` 在回测引擎内按日读取 `predictions/` 或**在线重算**（research 模式），保证与训练特征一致。

### 5.8 ML 相关 Skills

| Skill | 场景 | 步骤 |
|-------|------|------|
| `ml_feature_engineering.md` | 新建因子实验 | 查 manifest → query_feature_matrix → 检查缺失率 / PIT |
| `ml_train_evaluate.md` | 训练与验收 | 定 model_spec → train_model → evaluate_model → 解读 IC / 过拟合 |
| `ml_signal_integration.md` | 接入回测 | deploy_model（可选）→ run_ml_backtest → 对比纯因子 baseline |

### 5.9 ML 多 Agent 协作

```
Goal: "用价量+基本面特征预测沪深300未来5日超额收益，并回测 top20 Weekly 策略"
        │
        ▼
┌──────────────────┐
│ Orchestrator     │  Contract: PIT 无泄露、walk-forward、OOS Rank IC > 0.03
└────────┬─────────┘
         ▼
┌──────────────────┐
│ ML Executor      │  build_features → train_model → evaluate_model
│ agent: quant-ml  │  → run_ml_backtest
└────────┬─────────┘
         ▼
┌──────────────────┐
│ ML Verifier      │  检查: 禁运期、train-test gap、特征清单、
│ agent: quant-ml- │        是否偷看未来、样本量是否足够
│   verifier       │
└──────────────────┘
```

Verifier 重点审计**数据泄露**与**过拟合**，不看 Executor 的调参过程。

### 5.9.1 LLM 与 ML 的边界

| 由 LLM（Agent）做 | 由 ML 引擎做 |
|-------------------|-------------|
| 选择特征组合假设 | 矩阵运算、训练、推理 |
| 解读 IC / 回测报告 | walk-forward 切分 |
| 对比实验、撰写结论 | 模型持久化与版本 |
| 判断是否 deploy | 批量 predict |

**禁止**：让 LLM 直接输出训练代码并在本地 `exec()`；所有训练路径必须走 `model_spec` + 白名单模型类型。

### 5.10 模型注册表

每个模型版本目录：

```text
store/models/lgb_rank_5d_v1/
  model.pkl              # 或 model.txt (LightGBM native)
  metadata.json          # spec、训练区间、metrics、feature_hash
  feature_importance.json
  status: draft | staging | production
```

`deploy_model` 将某版本标为 `production` 前触发 Governance `ask_user`；仅 `production` 模型可被实盘 `strategy_spec` 引用。

---

## 6. 订单执行子系统

AKShare **不提供交易接口**。QuantStrike 通过 **OrderIntent → PreTrade 风控 → Governance → OrderRouter → ExecutionAdapter** 解决下单问题；回测、模拟盘、实盘共用同一套意图格式，只更换 Adapter。

### 6.1 问题与边界

| 层次 | 成交方式 | 数据来源 | 是否需要券商 |
|------|----------|----------|--------------|
| 回测 | 回测引擎按历史价模拟撮合 | Parquet 历史行情 | 否 |
| 模拟盘 | 本地账本 + AKShare 最新价估算成交 | 实时行情 | 否 |
| 实盘 | 券商 API 真实委托 | 券商回报 | 是 |

**Agent 不直接下单**：只产出 `OrderIntent` 或调用 `propose_order`；确定性 `OrderRouter` 负责校验、路由与落账。

### 6.2 总体链路

```
策略 / ML 信号 / 用户指令
            │
            ▼
     ┌─────────────┐
     │ OrderIntent │  声明式 JSON（标的、方向、数量、来源）
     └──────┬──────┘
            │
            ▼
     ┌─────────────┐
     │ PreTrade    │  纯 Python：T+1、涨跌停、整手、仓位上限、
     │ Risk Gate   │  交易时段、停牌、黑名单
     └──────┬──────┘
            │
            ▼
     ┌─────────────┐
     │ DeepStrike  │  paper → allow；live → ask_user + veto
     │ Governance  │
     └──────┬──────┘
            │
            ▼
     ┌─────────────┐
     │ OrderRouter │  execution/router.py
     └──────┬──────┘
            │
      ┌─────┼─────┬──────────┐
      ▼     ▼     ▼          ▼
  Backtest Paper Live    (future)
  Engine   Adapter Adapter
```

### 6.3 OrderIntent（订单意图）

Agent 或 cron 产出意图，**不等于**已向券商报单：

```json
{
  "intent_id": "ord_20260523_001",
  "symbol": "600519",
  "side": "buy",
  "quantity": 100,
  "order_type": "limit",
  "limit_price": 1680.0,
  "source": {
    "type": "ml_score",
    "model_id": "lgb_rank_5d_v1",
    "strategy_id": "ml_top20_rebalance"
  },
  "reason": "weekly rebalance top20 #3"
}
```

| 字段 | 说明 |
|------|------|
| `intent_id` | 幂等键，防重复下单 |
| `order_type` | `limit` / `market`（模拟盘 market 用最新价 + 滑点） |
| `source` | 审计：信号来自 ML、规则策略或人工 |
| `reason` | 供 Agent 报告与 SessionLog 审计 |

### 6.4 OrderRouter（确定性下单引擎）

```python
# execution/router.py — 不走 LLM
async def submit_intent(intent: OrderIntent, mode: str) -> OrderResult:
    errors = pre_trade_check(intent)       # execution/pre_trade.py
    if errors:
        return OrderResult(status="rejected", reason=errors)

    if mode == "backtest":
        raise ValueError("backtest uses BacktestEngine, not OrderRouter")
    if mode == "paper":
        return await paper_adapter.submit(intent)
    if mode == "live":
        return await live_adapter.submit(intent)
    raise ValueError(f"unknown mode: {mode}")
```

Tool 层薄封装：`place_paper_order` / `place_live_order` 内部调用 `submit_intent`，Governance 在 Tool 执行前拦截。

### 6.5 A 股 PreTrade 规则（硬编码，不可交给 LLM）

| 规则 | 处理 |
|------|------|
| **T+1** | 当日买入不可卖；卖单校验可用持仓 |
| **整手** | 数量须为 100 整数倍（科创板等例外写入配置） |
| **涨跌停** | 涨停拒买、跌停拒卖（或标记无法成交） |
| **交易时段** | 9:30–11:30、13:00–15:00；集合竞价策略单独配置 |
| **停牌** | AKShare 查状态，停牌拒单 |
| **单票仓位上限** | 如单票 ≤ 10% 总权益 |
| **行业集中度** | 可选，超阈值拒单或告警 |
| **黑名单** | `TRADING_BLACKLIST` + Governance veto |

#### 6.5.1 集合竞价与资金规则（A 股特有）

| 时段 / 规则 | PreTrade 行为 |
|-------------|---------------|
| **9:15–9:20** | 可报可撤 |
| **9:20–9:25** | 可报**不可撤**——`cancel_order` 硬拒绝，不传给券商 |
| **9:25–9:30** | 静默期，一般拒新单（策略可配置） |
| **卖出资金 T+0 可用** | 当日卖出所得可继续买入，但**不可取现** |
| **买入股票 T+1 可卖** | 卖单校验「可用持仓」= 昨仓 + 已成交卖单释放，不含当日买入 |

#### 6.5.2 废单 / 拒单与 QMT 异步回报

mini QMT / xtquant 回报可能延迟数秒。不能完全依赖券商查询的「可用余额」做连续批量下单，否则快速调仓时会出现**重复超额下单**。

### 6.6 本地 Pending Pool（冻结池）

`propose_order` 批准或 `place_*` 提交瞬间，**本地立刻冻结**对应资金或可用持仓，不等待券商异步回报：

```text
place_live_order(intent) 被批准后
    │
    ├─ pending_pool.freeze(cash | available_qty)   # execution/pending_pool.py
    ├─ 写 store/orders/{intent_id}.json  status=pending
    ├─ live_adapter.submit(intent) → 券商
    │
    ├─ 成交回报 → pending_pool.commit() → 更新 portfolio
    ├─ 拒单 / 废单 → pending_pool.release()
    └─ jobs/broker_sync.py 定期 reconcile 券商 vs 本地
```

| 状态 | 可用资金 | 可用持仓 |
|------|----------|----------|
| 冻结中 | 减去 pending 买单占用 | 减去 pending 卖单占用 |
| 成交 | commit 扣减 / 增加 | commit 更新 |
| 拒单 | release 归还 | release 归还 |

### 6.7 模拟盘（PaperAdapter）

不依赖券商，用 AKShare 报价 + 本地账本：

```text
place_paper_order(intent)
    │
    ├─ 读 store/portfolio/portfolio.json（现金、持仓、成本）
    ├─ fetch_realtime_quotes(symbol) 取参考价
    ├─ 模拟撮合：limit 可成交才成交，否则 status=pending
    ├─ 扣减现金 / 更新持仓 / 写 store/orders/{intent_id}.json
    └─ 返回成交摘要 JSON 给 Agent（非原始 tick）
```

**回测 vs 模拟盘**：

- **回测**：历史逐日批量验证策略逻辑
- **模拟盘**：实时或准实时跟踪「若现在下单会怎样」

模拟盘应配置 **滑点**（如 5–10 bps），使结果更接近实盘。

### 6.8 实盘：ExecutionAdapter 插件

```python
# execution/live/base.py
class ExecutionAdapter(Protocol):
    async def submit(self, intent: OrderIntent) -> OrderResult: ...
    async def cancel(self, broker_order_id: str) -> bool: ...
    async def get_positions(self) -> list[Position]: ...
    async def get_orders(self) -> list[Order]: ...
```

A 股常见后端选型：

| 方案 | 优点 | 缺点 |
|------|------|------|
| **mini QMT + xtquant** | 个人量化常用，API 较完整 | 需券商开通，多 Windows 环境 |
| **vnpy + 券商网关** | 开源、可扩展 | 部署维护成本高 |
| **easytrader** | 接入快 | 依赖客户端 UI，不稳定，不适合生产 |
| **掘金 / 聚宽 API** | 云端省心 | 非自建，与本地 DeepStrike 弱耦合 |

**建议路径**：P4 模拟盘验证 → P6 接 **mini QMT**（或已有券商 SDK），仅实现一个 `LiveAdapter`。

### 6.9 与 DeepStrike Governance 配合

```python
def make_trading_policy() -> Governance:
    gov = Governance(default="allow")

    # Agent 可提议，不可静默实盘
    gov.add_permission_rule("propose_order", "allow")
    gov.add_permission_rule("propose_rebalance", "allow")
    gov.add_permission_rule("place_paper_order", "allow")
    gov.add_rate_limit("place_paper_order", max_calls=50, window_ms=86_400_000)

    # 实盘必须人工批
    gov.add_permission_rule("place_live_order", "ask_user")
    gov.add_permission_rule("cancel_order", "ask_user")
    gov.add_permission_rule("rebalance", "ask_user")

    # 硬否决 + 参数约束
    gov.add_veto_rule(veto_non_trading_hours)
    gov.add_veto_rule(veto_blacklist)
    gov.limit_param_range("place_live_order", "amount", max_value=100_000)

    return gov
```

CLI / Web 收到 `permission_request` 时展示：标的、方向、数量、价格、策略来源、PreTrade 检查结果；用户批准后 `feed_tool_results` 继续执行。

### 6.10 推荐工作流

#### 模式 A：Human-in-the-loop（默认，最安全）

```text
15:30  predict → 生成 rebalance 目标权重
        │
        ▼
Agent: propose_rebalance → 输出「明日建议调仓表」
        │
        ▼
用户 UI 审阅 → 勾选 → place_paper_order / place_live_order
        │
        ▼
OrderRouter → Adapter → (券商)
        │
        ▼
post_trade_review skill → dream 沉淀
```

#### 模式 B：全自动模拟盘

```text
cron 09:25 → 读 production 模型 + 当前 portfolio
          → OrderRouter(paper) 自动 rebalance
          → 无需 ask_user
```

#### 模式 C：全自动实盘（不推荐初期）

```text
仅当 QUANT_MODE=live 且 deploy_model 已批准
→ 小仓位 + 严格 veto +  webhook 二次确认
```

### 6.11 订单状态机与工程问题

```
intent → pending → partial → filled
              └→ rejected / cancelled
```

| 问题 | 解法 |
|------|------|
| **重复下单** | `intent_id` 幂等 + Pending Pool 冻结；同 `(symbol, side, trade_date)` 去重 |
| **部分成交** | 状态机跟踪；`jobs/broker_sync.py` 定期 reconcile |
| **断线重启** | 启动时 `sync_broker_state()` + 重建 pending_pool |
| **Agent 误下单** | 实盘禁止 Agent 直调 `place_live_order`；仅 UI / 批准流触发 |
| **审计** | intent → order → fill 写 SessionLog + `store/orders/` |

### 6.12 从 ML 信号到下单

```text
predict → top_k 列表
    │
    ▼
propose_rebalance(target_weights from ML scores)
    │
    ▼
risk_review skill（集中度、止损、行业暴露）
    │
    ▼
place_paper_order / place_live_order（治理审批）
```

仅 `status=production` 的 ML 模型可驱动实盘 `strategy_spec`；模拟盘可用 `staging` 模型。

---

## 7. 治理策略

参照 `example/node` 中 FlashNote 的 `governance/policy.ts` 模式：

```python
def make_trading_policy() -> Governance:
    gov = Governance(default="allow")

    # 只读：放行
    for name in ["fetch_*", "compute_*", "screen_*", "get_*"]:
        gov.add_permission_rule(name, "allow")

    # ML：训练 / ML 回测限流；上线需审批
    gov.add_rate_limit("train_model", max_calls=3, window_ms=3_600_000)
    gov.add_rate_limit("run_ml_backtest", max_calls=5, window_ms=3_600_000)
    gov.add_permission_rule("deploy_model", "ask_user")

    # 回测：限流
    gov.add_rate_limit("run_backtest", max_calls=5, window_ms=3_600_000)

    # 执行：提议放行；实盘审批（详见 §6.9）
    gov.add_permission_rule("propose_order", "allow")
    gov.add_permission_rule("propose_rebalance", "allow")
    gov.add_rate_limit("place_paper_order", max_calls=50, window_ms=86_400_000)
    gov.add_permission_rule("place_live_order", "ask_user")
    gov.add_permission_rule("cancel_order", "ask_user")
    gov.add_permission_rule("rebalance", "ask_user")
    gov.limit_param_range("place_live_order", "amount", max_value=100_000)

    # 日内断路器（确定性，不可被 Agent 覆盖）
    gov.add_veto_rule(veto_market_crash)          # 指数大跌禁买
    gov.add_veto_rule(veto_daily_loss_limit)        # 日内累计亏损上限
    gov.add_veto_rule(veto_auction_cancel_window)  # 9:20-9:25 禁撤

    # 硬否决：黑名单标的、非交易时段
    gov.add_veto_rule(veto_non_trading_hours)
    gov.add_veto_rule(lambda name, args: is_blacklisted(args.get("symbol")))

    return gov


def veto_market_crash(name: str, args: dict) -> str | None:
    """沪深300当日跌幅超阈值时，否决所有买入实盘单。"""
    if name == "place_live_order" and args.get("side") == "buy":
        if get_hs300_daily_return() < -0.04:
            return "VETO: market crash circuit breaker (HS300 < -4%)"
    return None
```

除单笔 `max_value` 外，实盘还需配置**日内累计额度**与**日内最大亏损断路器**（见 `LIVE_DAILY_*` 环境变量）。完整下单链路见 **§6**。

---

## 8. Skills

Skill 文件只描述**流程与输出格式**；AKShare 字段细节放在 Tool docstring 或 Knowledge 中。

| Skill | 触发场景 | 核心步骤 |
|-------|----------|----------|
| `daily_market_scan.md` | 盘前 / 盘后 cron | 指数 → 板块 → 个股 → 资金流 → 输出 watchlist |
| `factor_screening.md` | 「找低估值高 ROE」类需求 | 定 universe → 拉财报 → 算因子 → 排序 → 解释 |
| `ml_feature_engineering.md` | 新建 ML 实验 | 查 manifest → query_feature_matrix → 检查 PIT / 缺失率 |
| `ml_train_evaluate.md` | 训练与验收 | model_spec → train_model → evaluate_model → 解读 IC |
| `ml_signal_integration.md` | ML 策略回测 | deploy_model → run_ml_backtest → 对比 baseline |
| `backtest_strategy.md` | 规则策略验证 | 声明 spec → run_backtest → 解读 Sharpe / 回撤 |
| `risk_review.md` | 下单前 | propose 后：集中度、行业暴露、止损、PreTrade 结果 |
| `post_trade_review.md` | 收盘后 | 计划 vs 实际成交 → 仅沉淀工程级教训（§9.2） |

---

## 9. Knowledge 与 Memory

### 9.1 分工

```
Knowledge（共享、只读）              Memory（Agent 专属、可写）
─────────────────────              ─────────────────────────
• 策略白皮书 / 因子定义               • 工程级教训（滑点、集合竞价未成交）
• ML 特征说明 / 标签定义              • 系统级故障与对账差异记录
• 交易规则（T+1、涨跌停、整手）          • 经 Harness 验证的策略统计结论
• 券商 Adapter 接入说明                 • 用户风险偏好（结构化）
• AKShare 字段说明                  • （禁止）短期观点型记忆
• 防泄露检查清单（PIT / embargo）
```

- **KnowledgeSource**：本地 Markdown + 向量检索
- **DreamStore**：会话后沉淀，但**严格限域**（见 §9.2）

### 9.2 DreamStore 防污染策略

量化研究中，短期失败经验长期可能有效，短期成功经验长期可能是灾难。无约束的 `runner.dream()` 会被**近期市场噪音（Recency Bias）**迅速污染。

| 允许写入 Memory | 禁止写入 Memory |
|-----------------|-----------------|
| 「600519 集合竞价滑点过大导致 pending」 | 「我觉得今天白酒板块不行」 |
| 「QMT 回报延迟 8s 导致 duplicate intent」 | 「最近追涨策略很有效」 |
| Harness 周度输出的统计显著结论 | Agent 自由生成的投研观点 |

**规则**：

1. **投研策略类教训**——仅由 `harness/strategy_judge` 在周 / 月回测完成后，以统计显著形式统写注入。
2. **Dream 默认关闭投研观点提取**——`dream()` 只提炼工程与执行层模式。
3. **时间衰减**——Memory 检索按 `created_at` 加权，超过 90 天的非工程记忆降权或归档。

---

## 10. 多 Agent 协作

适合 DeepStrike `VerificationContract` + `AgentPool`：

```
用户 Goal: "基于 ROE+低 PE 构建沪深300增强策略并回测"
        │
        ▼
┌──────────────────┐
│ Orchestrator     │  任务分解 + VerificationContract
│ agent: quant-    │  验收：universe 明确、Sharpe > 0.5、
│   orchestrator   │        最大回撤 < 20%、含风险说明
└────────┬─────────┘
         ▼
┌──────────────────┐
│ Executor         │  fetch → screen → run_backtest
│ agent: quant-    │  产出 BacktestReport artifact
│   researcher     │
└────────┬─────────┘
         │ HandoffArtifact
         ▼
┌──────────────────┐
│ Verifier         │  只看 artifact + contract
│ agent: quant-    │  pass/fail + driftRate + 改进建议
│   verifier       │
└──────────────────┘
```

Verifier 不看 Executor 对话历史，避免「维护既有结论」的认知偏差。

### 10.1 Artifact 完整性审计（防「假传圣旨」）

**风险**：Executor 回测不达标时，可能在 BacktestReport 文字中**粉饰 Sharpe / IC**；Verifier 若仅读 Executor 提交的 Markdown，会被 LLM 谄媚行为误导。

**防御**：确定性产物与 Agent 报告分离；Verifier 必须校验物理文件，而非信任 Executor  prose。

```
[Executor 调用 run_backtest / evaluate_model]
        │
        ▼ (确定性 Python，Agent 无写权限)
store/artifacts/{run_id}/
  result.json          # 原始指标
  manifest.json        # content_hash、tool_name、args_hash、created_at
        │
        ▼
[Executor 撰写解读报告] — 引用 run_id，不得改写数值
        │
        ▼
[Verifier 必须调用 verify_artifact_integrity(run_id)]
        │
        ├─ 重算 content_hash，比对 manifest
        ├─ 读取 result.json 中的 Sharpe / IC / net_ic_after_fees
        └─ 与 Executor 报告中的数字交叉验证 → pass / fail
```

| Tool | 说明 | 调用者 |
|------|------|--------|
| `verify_artifact_integrity` | 读物理文件 + 哈希比对 | **Verifier Agent**（Contract 必选） |
| `run_backtest` / `evaluate_model` | 写 `store/artifacts/`，返回 `run_id` | Executor |

HandoffArtifact 携带 `artifact_run_id` + `content_hash`；Contract 要求 Verifier 的 pass 必须附带 `verify_artifact_integrity` 的成功 ToolResult。

---

## 11. 信号驱动

```
SignalGateway
    │
    ├─ cron 09:00   → "执行 daily_market_scan，输出 watchlist"
    ├─ cron 09:25   → "paper 模式：production 模型自动 rebalance（见 §6.10B）"
    ├─ cron 15:30   → jobs/feature_pipeline.py 全量特征构建（非 Agent）
    ├─ cron 15:35   → predict production 模型，更新次日候选池
    ├─ cron 15:05   → "执行 post_trade_review，更新持仓复盘"
    ├─ cron 每 5min → "检查 price_alert 规则"
    └─ webhook      → 用户手动指令 / 第三方告警
              │
              ▼
       SignalRouter → RuntimeRunner.run(goal)
```

使用 DeepStrike `ScheduledPrompt`，配合 `market_calendar.py` 过滤非交易日。

---

## 12. 典型用户旅程

### A. 日常研究（单 Agent）

```text
Goal:
  加载 daily_market_scan skill。
  扫描今日 A 股：指数、行业资金流、北向资金。
  输出 JSON watchlist（≤10 只），含代码、理由、关键指标、风险点。

Tools: fetch_* → compute_* → screen_stocks
Governance: 全部 allow
After: dream 仅提取工程模式（§9.2），不写投研观点
```

### B. 策略回测（Harness 闭环）

```text
Goal + criteria:
  • 回测区间 ≥ 2 年
  • 报告含 CAGR、Sharpe、MaxDD、胜率
  • 策略逻辑与 spec 一致

Harness: strategy_judge (LLM-as-judge)
  前置确定性门槛：verify_artifact_integrity(run_id) 必须通过
  score < 0.8 → 重试 + feedback
  score ≥ 0.8 → 提取 Skill 候选
```

### C. 调仓下单（propose → 审批 → 执行）

```text
Goal:
  加载 risk_review skill。
  根据今日 ML top20 与当前 portfolio，propose_rebalance。
  输出建议调仓表；对卖出项检查 T+1 可用持仓。

用户 UI 批准卖出/买入项
  → place_paper_order（allow）或 place_live_order（permission_request）

OrderRouter → PaperAdapter / LiveAdapter
After: post_trade_review → dream
```

### D. ML 预测全流程（Harness + ML Verifier）

```text
Goal:
  加载 ml_train_evaluate skill。
  用 hs300 价量+基本面特征，预测未来 5 日超额收益。
  walk-forward 验证，OOS Rank IC 均值 > 0.03。
  通过后 run_ml_backtest（top20 周调仓），并与纯 ROE 因子 baseline 对比。

Tools:
  query_feature_matrix → build_labels → train_model
  → evaluate_model → run_ml_backtest

Harness 确定性门槛（LLM 之前拦截）:
  • net_ic_after_fees 达标
  • decay_ratio < 阈值
  • turnover 与成本匹配

Harness: strategy_judge + ML Verifier contract
  • verify_artifact_integrity(run_id) 必须通过
  • 无 PIT 泄露（Verifier 审计 feature_hash + 禁运期）
  • train-test IC gap < 0.02

After: 仅 Harness 周度统写可注入 DreamStore（§9.2）
```

---

## 13. 数据流与缓存

```
                    AKShare API
                         │
                         ▼
              ┌─────────────────────┐
              │  DataFetcher        │
              └──────────┬──────────┘
                         │
           ┌─────────────┴─────────────┐
           ▼                           ▼
      Parquet Cache          jobs/feature_pipeline (cron)
      (OHLCV)                         │
           │                           ▼
           │                    Feature Store
           └─────────────┬─────────────┘
                         │
           ┌─────────────┼─────────────┐
           ▼             ▼             ▼
      Predictions   Artifacts     Tool 摘要
      (cron)        (hash)        (给 LLM)
                         │
                    ML train / predict
                         │
                    Backtest Engine
                         │
              OrderIntent (propose)
                         │
              OrderRouter → Paper / Live
                         │
              store/orders/ + store/portfolio/
```

- **特征库由 cron 构建**，Agent 只读 `query_feature_matrix`
- 回测 / 评估产物写 `store/artifacts/` 并带 `content_hash`，Agent 不可篡改
- 订单审计链：intent → order → fill 与 SessionLog 关联
- 实时行情 TTL：30–60s；日线 TTL：当日有效

---

## 14. Runtime 组装示意

```python
from deepstrike import RuntimeRunner, RuntimeOptions, LocalExecutionPlane, collect_text

def make_quant_runtime(mode: str = "research") -> QuantRuntime:
    plane = LocalExecutionPlane()
    plane.register(
        fetch_daily_bars, fetch_realtime_quotes,
        compute_indicators, screen_stocks,
        query_feature_matrix, train_model, evaluate_model, predict,
        run_backtest, run_ml_backtest, get_backtest_report,
        verify_artifact_integrity,
        propose_order, propose_rebalance,
        get_portfolio, get_open_orders, place_paper_order, place_live_order,
        cancel_order, sync_broker_state,
    )

    runner = RuntimeRunner(RuntimeOptions(
        provider=make_provider(),
        execution_plane=plane,
        session_log=FileSessionLog("output/sessions"),
        skill_dir=SKILLS_DIR,
        knowledge_source=make_strategy_knowledge(),
        dream_store=make_file_dream_store(),
        governance=make_trading_policy(),
        agent_id=f"quant-{mode}",
        max_turns=15 if mode == "research" else 8,
        max_tokens=8192,
    ))
    return QuantRuntime(runner, plane)
```

---

## 15. 依赖

| 包 | 用途 |
|----|------|
| `deepstrike` | Agent 运行时（本仓库 `python/`） |
| `akshare` | A 股行情 / 基本面（**不含下单**） |
| `pandas` / `numpy` | 数据处理与指标 |
| `pyarrow` | Parquet 缓存 |
| `scikit-learn` | 基线模型、预处理、指标 |
| `lightgbm` | 默认 GBDT / Ranker（可选 `xgboost`） |
| `joblib` | 模型序列化 |
| `python-dotenv` | 环境变量 |
| `xtquant` / `vnpy` | 可选，实盘 ExecutionAdapter（P6） |

安装（规划，待 pyproject.toml 落地）：

```bash
cd example/python
python3 -m venv .venv && source .venv/bin/activate
pip install -e ".[quantstrike]"   # 或单独 pip install akshare pandas pyarrow
cp .env.example .env
```

---

## 16. 环境变量（规划）

| 变量 | 默认 | 说明 |
|------|------|------|
| `OPENAI_API_KEY` | — | LLM Provider（或兼容 API Key） |
| `MODEL` | `gpt-4o` | 模型名称 |
| `OPENAI_BASE_URL` | OpenAI 默认 | 本地 / 代理 |
| `QUANT_MODE` | `research` | `research` / `backtest` / `paper` / `live` |
| `TRADING_BLACKLIST` | — | 逗号分隔标的代码 |
| `ML_DEFAULT_MODEL` | `lightgbm` | 默认 ML 后端 |
| `ML_PRODUCTION_MODEL_ID` | — | 当前 production 模型 ID |
| `EXECUTION_ADAPTER` | `paper` | `paper` / `qmt` / `vnpy` |
| `PAPER_INITIAL_CASH` | `1000000` | 模拟盘初始资金 |
| `PAPER_SLIPPAGE_BPS` | `10` | 模拟盘滑点（基点） |
| `LIVE_MAX_ORDER_AMOUNT` | `100000` | 单笔实盘金额上限 |
| `LIVE_DAILY_BUY_LIMIT` | `500000` | 日内累计买入上限 |
| `LIVE_DAILY_LOSS_LIMIT` | `0.03` | 日内亏损断路器（占权益比例） |
| `HS300_CRASH_THRESHOLD` | `-0.04` | 指数跌幅超此值禁买 |
| `PORT` | `3000` | Web UI 端口（可选） |

---

## 17. 分阶段落地

> **推荐第一步（P0–P1）**：先不做 ML。实现 `store/parquet_cache.py` + `skills/daily_market_scan.md`，让单 Agent 每个交易日收盘后稳定抓取 AKShare 数据、产出标准 Markdown 复盘报告并落盘 SessionLog。把 **I/O 链路 + 本地持久化**跑通后，再叠 ML 预测与订单路由。

| 阶段 | 交付物 | 建议周期 |
|------|--------|----------|
| **P0 数据层** | AKShare Tools + Parquet 缓存 + `output/reports/` | 1 周 |
| **P1 研究 Agent** | daily_market_scan + 收盘复盘 CLI（单 Agent 闭环） | 1 周 |
| **P1b 特征 Cron** | `jobs/feature_pipeline.py` + feature_store manifest | 3–5 天 |
| **P2a ML 只读** | `query_feature_matrix` + PIT 对齐 | 1 周 |
| **P2b ML 训练** | model_spec + walk-forward + train/evaluate Tools | 1–2 周 |
| **P2c ML 回测** | run_ml_backtest + signal 接入 strategy_spec | 1 周 |
| **P3 规则回测** | 传统 strategy_spec + backtest engine + Harness | 1 周 |
| **P4a 下单基础** | OrderIntent + pre_trade + Pending Pool + OrderRouter | 1 周 |
| **P4b 模拟盘** | PaperAdapter + portfolio/orders + propose/place Tools | 1 周 |
| **P4c 调仓 cron** | propose_rebalance + risk_review + paper 自动调仓 | 3–5 天 |
| **P5 多 Agent** | Verifier + `verify_artifact_integrity` + Orchestrator | 1–2 周 |
| **P6 实盘** | LiveAdapter（QMT/vnpy）+ ask_user 审批流 + sync | 视券商而定 |

---

## 18. 关键设计决策

1. **AKShare 不下单**——实盘必须接 ExecutionAdapter（QMT / vnpy 等）。
2. **Agent 只 propose，不静默实盘**——`place_live_order` 走 `ask_user`；推荐 UI 批准流。
3. **LLM 不训练、不推理、不写 artifact**——数值产物由确定性模块写入，带 `content_hash`。
4. **策略 / 模型 / 订单均用 JSON spec**——禁止 LLM 生成可执行代码或直接调券商。
5. **PreTrade + Pending Pool 硬编码 A 股规则**——含集合竞价撤单窗口、T+0 资金 / T+1 股票。
6. **特征 Cron 构建、Agent 只读**——禁止 Agent 按需触发全 universe rolling 计算。
7. **PIT + embargo 是第一公民**——特征、标签、验证共用同一套防泄露逻辑。
8. **费后 IC 为 Harness 硬门槛**——`net_ic_after_fees` 不达标则禁止 deploy 讨论。
9. **Verifier 校验物理 artifact**——不信任 Executor  prose，必须 `verify_artifact_integrity`。
10. **DreamStore 限域**——只记工程教训 + Harness 统计结论；禁止短期观点型记忆。
11. **实盘断路器**——指数大跌禁买、日内亏损上限、日内累计额度。
12. **walk-forward 为默认验证**——不用随机 K-Fold 作为主评估手段。
13. **deploy_model 必须 `ask_user`**——production 模型才能驱动实盘策略。

---

## 19. 参考

- [DeepStrike 架构文档](https://github.com/kongusen/deepstrike/blob/main/docs/architecture.md)
- [DeepStrike Python SDK](https://github.com/kongusen/deepstrike/blob/main/python/README.md)
- [FlashNote 示例](https://github.com/kongusen/deepstrike/blob/main/example/node/README.md)（Node 版参考：runtime / governance / skills 模式）
- [MeetingMind 示例](https://github.com/kongusen/deepstrike/blob/main/example/python/README.md)（Python 版参考：store / harness / signals 模式）
- [AKShare 文档](https://akshare.akfamily.xyz/)
