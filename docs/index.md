---
layout: home

hero:
  name: DeepStrike
  text: Agent 运行时内核
  tagline: 可重放状态、受治理工具、跨语言 SDK — 让动态工作流可生产化。
  image:
    src: /banner.png
    alt: DeepStrike
  actions:
    - theme: brand
      text: 5 分钟上手
      link: /getting-started/hello-agent
    - theme: alt
      text: 什么是 Agent OS
      link: /architecture/agent-os
    - theme: alt
      text: 架构总览
      link: /architecture/
    - theme: alt
      text: GitHub Wiki
      link: https://github.com/kongusen/deepstrike/wiki

features:
  - icon: 🧠
    title: Kernel + SDK 分层
    details: Rust 纯计算内核；Python / Node / WASM SDK 负责 I/O。SDK 喂事件，内核返动作。
  - icon: 🕸️
    title: 动态工作流
    details: 声明式 DAG + 运行时 append + Loop / Classify / Tournament 控制流节点。
  - icon: 📦
    title: Context 工程
    details: 四槽位渲染、压力压缩、Handle 分页、Prompt Cache 感知。
  - icon: 🛡️
    title: 内核级治理
    details: Syscall trap、配额、rate limit、rollback note — 不是 SDK 事后拦截。
  - icon: 💾
    title: Memory Syscall
    details: writeMemory / queryMemory 内核校验 + DreamStore + 空闲整理管线。
  - icon: 🤝
    title: 多 Agent 协作
    details: Sub-agent 隔离、ReactiveSession、Contract、Handoff。
---

## 支持的 SDK

::: code-group

```bash [Python]
pip install deepstrike
```

```bash [Node.js / TS]
npm install @deepstrike/sdk
```

```toml [Rust]
[dependencies]
deepstrike-sdk = "0.2"
```

```bash [WASM]
npm install @deepstrike/wasm
```

:::

## 阅读路径

| 你是谁 | 从这里开始 |
|--------|-----------|
| 新用户 | [Hello Agent](./getting-started/hello-agent) |
| 集成开发者 | [API 选型](./getting-started/run-agent-vs-runner) → [功能指南](./guides/) |
| 架构评审 | [Kernel / SDK 分层](./architecture/overview) |
| Wiki 读者 | [GitHub Wiki](https://github.com/kongusen/deepstrike/wiki)（与 `docs/` 同步） |

## 文档站点

- **VitePress**（本站点）：`npm run docs:dev` 本地预览；顶部可切换 **简体中文 / English**
- **GitHub Wiki**：`docs/` 变更后由 CI 同步（中文 + `En-*` 英文页），见 [Wiki 同步说明](./wiki/README.md)
