# 入门

DeepStrike 帮你构建能够使用工具、记住事实、协作委托，并跨 Session 持续工作的 Agent。

## 推荐路径

1. 为 Python、Node.js、Rust 或 WASM [安装 SDK](./installation)。
2. 使用一个工具运行 [Hello Agent](./hello-agent)。
3. 在 [API 选型](./run-agent-vs-runner) 中使用统一的 `createAgent`、`agent.run` 和 `agent.stream`。
4. 接入一个 [Provider](./providers)。
5. 从 [Agent 能力指南](/guides/) 中添加 Agent 需要的能力。

## 选择入口

| API | 适合场景 |
| --- | --- |
| `createAgent()` + `agent.run()` | 执行一次目标并返回结构化结果。 |
| `agent.stream()` | 获取流式事件。 |
| `agent.session()` / `agent.workflow()` | 使用连续会话和多 Agent 编排。 |

## 边做边学

[Research Brief Studio 示例](https://github.com/kongusen/deepstrike/tree/main/example) 提供八级路径，从带来源的问答 Agent 逐步扩展到 Reactive 编辑团队。
