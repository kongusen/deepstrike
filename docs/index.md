---
layout: home

hero:
  name: DeepStrike
  text: 让 Agent 持续完成工作
  tagline: 为 Agent 配置工具、记忆、Skill、委托、工作流和可恢复 Session。
  image:
    src: /banner.png
    alt: DeepStrike
  actions:
    - theme: brand
      text: 5 分钟上手
      link: /getting-started/hello-agent
    - theme: alt
      text: Agent 能力
      link: /guides/
    - theme: alt
      text: 教程课程
      link: https://github.com/kongusen/deepstrike/tree/main/example
    - theme: alt
      text: GitHub Wiki
      link: https://github.com/kongusen/deepstrike/wiki

features:
  - icon: 🧠
    title: 接入任意 Provider 推理
    details: 选择适合应用的模型，支持流式输出、多模态输入、路由和可重放测试。
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
    title: 跨时间持续工作
    details: 等待审批或外部事件，进程重启后恢复 Session，并用有界循环推进长任务。
  - icon: ✅
    title: 让行为可预期
    details: 使用工具策略、配额、输出 schema、评估 gate 和结构化运行证据。
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
