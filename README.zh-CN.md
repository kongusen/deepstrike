<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>为高能力 Agent 提供持久化与治理能力的运行框架。</strong>
</p>

<p align="center">
  <a href="https://github.com/kongusen/deepstrike/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kongusen/deepstrike?sort=semver&style=for-the-badge&label=release&labelColor=111827&color=374151"></a>
  <a href="https://www.npmjs.com/package/@deepstrike/sdk"><img alt="npm" src="https://img.shields.io/npm/v/@deepstrike/sdk?style=for-the-badge&logo=npm&logoColor=white&label=npm&labelColor=111827&color=374151"></a>
  <a href="https://pypi.org/project/deepstrike/"><img alt="PyPI" src="https://img.shields.io/pypi/v/deepstrike?style=for-the-badge&logo=pypi&logoColor=white&label=pypi&labelColor=111827&color=374151"></a>
  <a href="https://crates.io/crates/deepstrike-sdk"><img alt="crates.io" src="https://img.shields.io/crates/v/deepstrike-sdk?style=for-the-badge&logo=rust&logoColor=white&label=crates&labelColor=111827&color=374151"></a>
  <a href="https://discord.gg/cwS3RBYCv"><img alt="Discord" src="https://img.shields.io/badge/discord-community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=111827"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-374151?style=for-the-badge&labelColor=111827"></a>
</p>

<p align="center">
  <strong>中文</strong>
  · <a href="./README.md">English</a>
  · <a href="./docs/index.md">文档</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

---

DeepStrike 用来构建不止会回答 prompt 的 Agent。你可以为 Agent 配置模型、指令、工具、MCP Server、Skill、Memory、Knowledge 和 Handoff，让它跨多个回合工作，协调其他 Agent，等待外部输入，并在中断后继续运行。

框架保留熟悉的 Agent 表达方式，同时把周围的能力做成清晰、可组合、可测试的接口。

## Agent 能做什么

| Agent 能力 | DeepStrike 提供 |
| --- | --- |
| **推理** | 支持 OpenAI、Anthropic、Gemini、DeepSeek、Kimi、Qwen、GLM、Minimax、Ollama 和自定义 Provider，并支持流式输出与 replay。 |
| **使用工具** | 类型化工具、流式工具、MCP 集成、本地执行、worktree、进程沙箱和远程工具适配。 |
| **记忆** | 当前运行的 Working Memory、MemoryStore 持久记忆、Session 提取、检索和受策略约束的写入。 |
| **加载知识** | 按需加载 Skill 与 Knowledge，在运行阶段固定、限制预算，并在任务切换后释放。 |
| **委托工作** | 创建具有明确角色、收窄权限、隔离上下文、handoff、contract 和 lineage 的子 Agent。 |
| **协作编排** | 并行 fan-out、综合、依赖图、分类器、reducer、验证 gate、tournament 和有界循环。 |
| **等待与唤醒** | 等待审批、子 Agent 完成和外部信号，使用可恢复 Session 继续执行，不需要手动重建整段对话。 |
| **遵守限制** | 工具策略、参数约束、审批 gate、速率限制、配额、预算和取消规则。 |
| **处理长上下文** | 分开管理稳定指令、Knowledge、对话历史和运行状态，支持压缩与大结果分页，让长任务保持可用。 |
| **解释执行过程** | Session Log、结构化事件、replay fixture、snapshot 和恢复证据，方便调试与审计。 |

这些能力围绕同一套 Agent contract 组合。你可以从一次工具调用开始，按需增加 Memory、Skill、委托、工作流控制和持久化。

## 快速开始

安装 Node.js SDK。

```bash
npm install @deepstrike/sdk
```

运行一个使用类型化工具并保存本地 Session Log 的 Agent。

```ts
import {
  FileSessionLog,
  LocalExecutionPlane,
  OpenAIResponsesProvider,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const runner = new RuntimeRunner({
  provider: new OpenAIResponsesProvider(process.env.OPENAI_API_KEY!, "gpt-5-mini"),
  executionPlane: new LocalExecutionPlane().register(add),
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
})

const answer = await collectText(runner.run({
  sessionId: "math-1",
  goal: "What is 17 + 28?",
}))

console.log(answer)
```

按 Agent 的复杂度选择入口。

| 需求 | 入口 |
| --- | --- |
| 一个目标，可选工具和最终文本 | `runAgent` 或 `run_agent` |
| 多个 Agent 并行工作后综合 | `runFanout` 或 `run_fanout` |
| 流式事件、Session、Memory、信号、治理或显式工作流 | `RuntimeRunner` |

各语言的安装与完整示例见 [Node.js](./node/README.md)、[Python](./python/README.md)、[Rust](./rust/README.md) 和 [WASM](./wasm/README.md)。第一步可以从 [Hello Agent](./docs/getting-started/hello-agent.md) 开始。

## 常见 Agent 模式

### 使用工具的单 Agent

注册类型化工具，让 Agent 自己决定何时调用。工具 schema、执行结果、错误和流式事件都属于同一次运行。

### 记忆助手

接入 `MemoryStore`，召回用户或项目的持久事实，按策略写入新记忆，并在 Session 结束时提取有价值的记录。

### Skill Agent

把专业指令和工具放进 Skill。任务需要时加载 Skill，在当前阶段保留相关知识，切换任务后释放它。

### 多 Agent 工作流

把任务描述成一张图。让研究 Agent 并行工作，用确定性 reducer 合并结果，再让验证 Agent 质疑结论，最后综合输出。

### 长时间运行的 Agent

持久化 Session，在审批或外部事件处暂停，使用 `wake(sessionId)` 继续执行。Checkpoint 和 replay 证据让应用可以在进程重启后恢复。

[Research Brief Studio 课程](./example/README.md) 用八个可运行等级展示这些模式，从带来源的问答 Agent 到受治理的多 Agent 编辑室。每个等级都提供 `--dry-run`，不需要 Provider 凭据。

## 适用场景

DeepStrike 适合需要持久 Session、受控工具、Memory、委托、动态工作流，或需要在 Node.js、Python、Rust 和 WASM 之间保持一致行为的应用。

对于无状态聊天接口或没有工具的一次性 prompt，Provider SDK 可能已经足够。当 Agent 需要持续工作、协调其他 Agent、遵守限制，或者需要解释和恢复自己的执行过程时，再引入 DeepStrike。

## 当前范围

当前重点是可靠的本地 Agent runtime。它支持本地持久化、replay、恢复、进程监督、确定性工作流调度、权限限制，以及由应用提供的远程工具和 MCP Server 集成。

框架当前不承诺远程 worker lease、任务迁移、分布式接管或分布式消息 broker。外部 effect 采用 at-least-once 语义，数据库、邮件、支付等系统应由应用使用幂等键和 reconciliation。

Billing、pricing、税务和租户计费属于使用 DeepStrike 的应用。框架可以提供使用量信息和运行时事件，但不定义产品计费策略。

## 文档

| 你想要做什么 | 从这里开始 |
| --- | --- |
| 了解 Agent 模型 | [快速开始](./docs/getting-started/index.md) |
| 选择 API | [runAgent 与 RuntimeRunner](./docs/getting-started/run-agent-vs-runner.md) |
| 构建工作流 | [动态工作流](./docs/guides/workflow.md) |
| 添加工具和集成 | [Execution Plane 与 Tools](./docs/guides/execution-plane-and-tools.md) 和 [Provider 路由](./docs/guides/provider-routing.md) |
| 添加治理 | [治理](./docs/guides/governance.md) |
| 添加 Skill 和 Memory | [Skill](./docs/guides/skills.md) 和 [Memory](./docs/guides/memory.md) |
| 构建可恢复运行 | [Session、Replay 与恢复](./docs/guides/session-replay-and-recovery.md) |
| 查看完整 API | [Reference](./docs/reference/index.md) |

## 仓库结构

```text
crates/deepstrike-core/   共享运行时实现
crates/deepstrike-node/   Node.js 原生绑定
crates/deepstrike-py/     Python 原生绑定
crates/deepstrike-wasm/   WASM 绑定
node/                     TypeScript SDK
python/                   Python SDK
rust/                     Rust SDK
wasm/                     Browser 与 Edge SDK
example/                  可运行的 Agent 课程
docs/                     VitePress 文档源文件
tests/                    跨语言 contract 与 fixture
```

## 开发

要求 Rust 1.85+、Node.js 18+ 与 Python 3.10+。

```bash
cargo test
npm run docs:build
```

各 SDK 的开发命令见上方对应语言 README。提交 PR 前请阅读 [CONTRIBUTING.md](./CONTRIBUTING.md)，安全漏洞请按 [SECURITY.md](./SECURITY.md) 的流程报告。

## 许可证

DeepStrike 使用 [MIT License](./LICENSE)。这是一个独立的开源项目，不隶属于任何模型提供商，也未获得任何模型提供商背书。
