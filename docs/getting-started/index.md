# 入门

欢迎使用 DeepStrike — 面向生产环境的 Agent 运行时框架。

## 推荐阅读顺序

1. [安装](./installation) — Python / Node / Rust
2. [Hello Agent](./hello-agent) — 第一个可运行 agent
3. [API 选型](./run-agent-vs-runner) — `run_agent` vs `RuntimeRunner` vs `run_fanout`
4. [Provider](./providers) — 接入 LLM

## 三种 API 层级

| API | 场景 |
|-----|------|
| `run_agent()` | 单 prompt → 返回文本（90% 场景） |
| `run_fanout()` | 并行 N 任务 + 合成 |
| `RuntimeRunner` | 流式事件、信号、记忆、治理、工作流 |

源码：`python/deepstrike/runtime/facade.py`
