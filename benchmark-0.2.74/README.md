# DeepStrike Node SDK 0.2.74 Benchmark

这是面向 Node SDK `0.2.74` 的新测评目录。它按 SDK 的新契约和 API 分层重建，保留旧的 `benchmark/` 目录作为历史实现，不复用旧 runner、旧 loader 或旧场景。

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
npm test --prefix benchmark-0.2.74
```

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
- `tests/`：Node built-in test。
- `runs/`：本地运行输出，默认不提交。
- `baselines/`：被接受的 golden JSON。

真实 provider 测评可以在后续增加为显式的 `live` 模式；默认测评不依赖 API key，也不把 live trace 混入机制基线。
