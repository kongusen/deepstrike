---
layout: home

hero:
  name: DeepStrike
  text: 本地 Agent Process Runtime
  tagline: 用可持久化的进程树、权限、预算、调度、通信与恢复，让 Agent 持续完成工作。
  image:
    src: /banner.png
    alt: DeepStrike
  actions:
    - theme: brand
      text: 了解 Agent Runtime
      link: /architecture/agent-process-runtime
    - theme: alt
      text: 5 分钟上手
      link: /getting-started/hello-agent
    - theme: alt
      text: 教程课程
      link: https://github.com/kongusen/deepstrike/tree/main/example
    - theme: alt
      text: GitHub Wiki
      link: https://github.com/kongusen/deepstrike/wiki

features:
  - icon: ⚙️
    title: Agent Process Runtime
    details: 把根 Agent、子 Agent 和 Workflow 节点统一为具有谱系、生命周期和监督策略的本地运行时任务。
  - icon: 🧰
    title: 使用真实工具
    details: 添加类型化工具、流式工具、MCP Server、文件、沙箱、worktree 和应用自有集成。
  - icon: 🧠
    title: 记忆与学习
    details: 组合 Working Memory、持久 Memory、Skill 和 Knowledge，不把每次对话都塞进 prompt。
  - icon: 🤝
    title: 委托与协作
    details: 运行专注的子 Agent，传递 handoff，扇出研究任务，验证结果，并构建 Reactive peer 团队。
  - icon: ⏳
    title: 持久等待与恢复
    details: 等待 Effect、子任务、审批、Signal 或 Timer，进程重启后从 checkpoint 和 replay 继续。
  - icon: ✅
    title: 权限、预算与确定性
    details: 收窄子任务 capability，逐级分配资源预算，并用统一 runnable set 保持调度可预测。
---

## 从你要构建的 Agent 开始

::: code-group

```bash [Python]
pip install deepstrike
```

```bash [Node.js / TypeScript]
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

| 你要构建…… | 从这里开始 |
| --- | --- |
| 理解 Agent 的运行时模型 | [Agent Process Runtime](/architecture/agent-process-runtime) |
| 一个使用工具的 Agent | [Hello Agent](/getting-started/hello-agent) |
| 一个拥有 Memory 或 Skill 的 Agent | [Agent 能力](/guides/) |
| 一个多 Agent 工作流 | [动态工作流](/guides/workflow) |
| 一个可恢复的长时间运行 Agent | [Session 与恢复](/guides/session-replay-and-recovery) |
| 一个协作式 Agent 团队 | [Sub-Agent 与协作](/guides/sub-agents-and-collaboration) |
| 完整 API | [参考](/reference/) |

## 边做边学

[Research Brief Studio 课程](https://github.com/kongusen/deepstrike/tree/main/example) 用八个可运行等级逐步扩展同一个产品。

1. 使用来源和工具回答问题。
2. 跨 Session 召回持久事实。
3. 加载 Skill 和任务相关 Knowledge。
4. 响应 Signal 和变化中的输入。
5. 应用策略、审批和资源限制。
6. 让长时间运行的循环自定节奏并在之后唤醒。
7. 扇出给专业 Agent，验证结构化结果。
8. 让多个 peer 围绕同一个编辑任务协作。

每一级都有 TypeScript 和 Python 示例，并提供 `--dry-run`，无需 Provider 凭据即可验证接线。

## 文档渠道

- **VitePress**：运行 `npm run docs:dev` 本地预览，顶部可以切换中文和 English。
- **GitHub Wiki**：由 CI 根据 `docs/` 自动生成，见 [Wiki 同步说明](./wiki/README.md)。
