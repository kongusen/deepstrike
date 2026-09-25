# DeepStrike Node SDK 0.2.74 Benchmark

这是面向 Node SDK `0.2.74` 的测评目录，按 SDK 的新契约和 API 分层重建。

## 测评分层

| 层 | 覆盖内容 | 默认方式 |
| --- | --- | --- |
| Public contract | root intent API、`providers`、`workflow`、`planes`、`memory`、`harness`、`os`、`advanced`、`runtime`、`evals` 的 export map 与 root leakage | 直接加载 `node/dist`，无网络 |
| Behavior | Agent facade、memory boundary、workflow、dynamic workflow、execution plane、AttemptLoop、eval trace | stub orchestrator / ReplayProvider |
| Replay and regression | dynamic workflow invocation replay、JSON artifact、metric diff、golden baseline | 确定性 fixture |

## 使用

```bash
npm run build --prefix node
node benchmark-0.2.74/cli/bench.mjs list
node benchmark-0.2.74/cli/bench.mjs contract-surface
node benchmark-0.2.74/cli/bench.mjs agent-facade
node benchmark-0.2.74/cli/bench.mjs planes-harness-evals
node benchmark-0.2.74/cli/bench.mjs workflow --compare
node benchmark-0.2.74/cli/bench.mjs dynamic-complex
node benchmark-0.2.74/cli/bench.mjs skill-progressive
npm test --prefix benchmark-0.2.74
```

真实 provider 测评必须显式开启。它读取项目根目录 `.env`，只输出脱敏状态和指标：

```bash
node benchmark-0.2.74/cli/bench.mjs live-smoke --live --provider=all
# 只测一个 provider
node benchmark-0.2.74/cli/bench.mjs live-smoke --live --provider=openai
# 限制单次请求时长和累计 token
node benchmark-0.2.74/cli/bench.mjs live-smoke --live --provider=openai --timeout-ms=90000 --max-total-tokens=1200
# 使用 OpenAI key 详细验证功能矩阵
node benchmark-0.2.74/cli/bench.mjs live-comprehensive --live --provider=openai --max-total-tokens=1200
# 只重跑未触发的能力
node benchmark-0.2.74/cli/bench.mjs live-comprehensive --live --provider=openai --features=memory,skill --max-total-tokens=1200
# 用真实 provider key 驱动模型激活 Anthropic Agent Skills fixture
node benchmark-0.2.74/cli/bench.mjs live-skill-progressive --live --provider=minimax --timeout-ms=90000 --max-total-tokens=2000
# 也可切换到 OpenAI 或 Kimi
node benchmark-0.2.74/cli/bench.mjs live-skill-progressive --live --provider=kimi --timeout-ms=90000 --max-total-tokens=2000
```

live smoke 会检查真实鉴权、一次普通 run、一次 stream、usage/evidence，以及模型是否实际完成工具调用。工具调用没有发生时会记录为 `not_exercised`，不会把模型能力差异误报成 SDK 失败。

`skill-progressive` 使用标准 `SKILL.md`、`references/`、`assets/`、`scripts/`、`examples/` 目录，验证 metadata → skill body → 按需资源的渐进式加载，以及 `allowed_tools` 带来的工具面扩大。`live-skill-progressive` 用真实 provider 请求重复这个过程，可选择 OpenAI、MiniMax 或 Kimi。

`dynamic-complex` 覆盖动态 workflow 的 phase、串行 pipeline、有并发上限的 fan-out、条件分支、聚合、审批、pause/resume 和 replay。

复杂业务流程的体系化优化、优先级和验收矩阵见 [`COMPLEX-FLOW-OPTIMIZATION.md`](./COMPLEX-FLOW-OPTIMIZATION.md)。

`live-comprehensive` 进一步覆盖 session、tool、memory host/API 与 memory tool、knowledge tool、skill loader/declaration、output schema、SignalGateway、PermissionManager，以及真实 `RuntimeRunner.runDynamicWorkflow()` 的并行子 agent 和生命周期事件。
报告会把“工具已执行但模型最终回答未在预算内完成”记为 `completionWarnings`，保留能力执行证据，不把它隐藏成普通成功。

保存和检查 baseline：

```bash
node benchmark-0.2.74/cli/bench.mjs workflow --variant=default --baseline-save
node benchmark-0.2.74/cli/bench.mjs workflow --variant=default --baseline-check
```

## 目录

- `contracts/manifest.mjs`：0.2.74 公开 barrel 的必需/禁止符号契约。
- `core/sdk.mjs`：唯一 SDK loader 和版本守卫。
- `core/runner.mjs`：场景执行、artifact、compare、baseline。
- `scenarios/`：确定性契约与行为场景。
- `fixtures/anthropic-progressive-skill/`：复杂 Anthropic Agent Skills fixture，包含 `SKILL.md` 和懒加载资源。
- `tests/`：Node built-in test。
- `runs/`：本地运行输出，默认不提交。
- `baselines/`：被接受的 golden JSON。

默认测评不依赖 API key，也不把 live trace 混入机制基线。

跨 SDK 的协议契约以根目录 `VERSION` 为唯一版本源，Node、Python、Rust 和 WASM 共用 `contracts/manifests/` 生成物。对齐和检查命令：

```bash
npm run contracts:verify
node scripts/check-sdk-parity.mjs
node scripts/run-sdk-conformance.mjs --validate-only
```

其中 `contracts:verify` 检查 host、kernel、provider、workflow、memory、skill、signal 和 replay 边界 manifest；`check-sdk-parity` 检查各 SDK 的实现标记；`run-sdk-conformance` 使用同一组 canonical fixtures 做跨 SDK 行为比较。SDK 专属能力保留在各自的 public surface，跨 SDK 只对齐共享协议和可观察行为。
