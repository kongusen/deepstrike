<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>面向动态工作流、受治理工具、可重放会话与跨语言 Agent Runtime 的 Agent OS 微内核。</strong>
</p>

<p align="center">
  <a href="https://github.com/kongusen/deepstrike/releases"><img alt="Release" src="https://img.shields.io/github/v/release/kongusen/deepstrike?sort=semver&style=for-the-badge&label=release&labelColor=111827&color=374151"></a>
  <a href="https://www.npmjs.com/package/@deepstrike/sdk"><img alt="npm" src="https://img.shields.io/npm/v/@deepstrike/sdk?style=for-the-badge&logo=npm&logoColor=white&label=npm&labelColor=111827&color=374151"></a>
  <a href="https://pypi.org/project/deepstrike/"><img alt="PyPI" src="https://img.shields.io/pypi/v/deepstrike?style=for-the-badge&logo=pypi&logoColor=white&label=pypi&labelColor=111827&color=374151"></a>
  <a href="https://crates.io/crates/deepstrike-sdk"><img alt="crates.io" src="https://img.shields.io/crates/v/deepstrike-sdk?style=for-the-badge&logo=rust&logoColor=white&label=crates&labelColor=111827&color=374151"></a>
  <a href="https://www.anthropic.com/claude"><img alt="Optimized with Fable 5" src="https://img.shields.io/badge/optimized%20with-Fable%205-D97757?style=for-the-badge&logo=anthropic&logoColor=white&labelColor=111827"></a>
  <a href="https://discord.gg/cwS3RBYCv"><img alt="Discord" src="https://img.shields.io/badge/discord-community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=111827"></a>
  <a href="https://x.com/w73775"><img alt="在 X 上关注 @w73775" src="https://img.shields.io/badge/follow-%40w73775-000000?style=for-the-badge&logo=x&logoColor=white&labelColor=111827"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-374151?style=for-the-badge&labelColor=111827"></a>
</p>

<p align="center">
  <strong>中文</strong>
  · <a href="./README.md">English</a>
</p>

<p align="center">
  <a href="./docs/index.md">文档</a>
  · <a href="./docs/getting-started/hello-agent.md">Hello Agent</a>
  · <a href="./docs/architecture/agent-os.md">Agent OS</a>
  · <a href="./docs/guides/workflow.md">动态工作流</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

<details>
<summary><strong>浏览 DeepStrike</strong></summary>

- [从这里开始](#从这里开始)
- [看看它如何工作](#看看它如何工作)
- [你会得到什么](#你会得到什么)
- [为什么是内核？](#为什么是内核)
- [快速开始](#快速开始)
- [动态工作流模式](#动态工作流模式)
- [什么时候适合用 DeepStrike](#什么时候适合用-deepstrike)
- [文档](#文档)
- [常见问题](#常见问题)

</details>

---

DeepStrike 把 Agent 的「harness」升级为内核原语。

复杂任务里的现代 Agent 越来越像在写一个小型工作流：先分类，再扇出给多个子 Agent，校验输出，循环直到完成，最后综合结果。脚本里的 harness 很灵活，但也很脆：状态常在进程内存里，治理靠临时判断，中断恢复困难，每种语言还要重复实现一遍语义。

DeepStrike 把控制面放进 `deepstrike-core` 这个纯 Rust 状态机。宿主 SDK 仍然拥有所有真实 I/O：LLM 调用、工具、文件、worktree、网络、长期记忆与存储。内核决定 effect 何时、是否、在什么预算内发生；宿主执行获批的 effect，再把 observation 回灌给内核。

<p align="center">
  <img src="docs/public/readme_agent_os_map_zh.svg" alt="DeepStrike 运行机制：宿主 I/O、RuntimeRunner、内核、四语言 SDK 与 Self-Harness 外环" width="100%" />
</p>

## 从这里开始

DeepStrike 面向希望获得 Agent 自主性、但**不想把最终权限交给 prompt** 的构建者。按你正在解决的问题选择入口：

| 我想要…… | 从这里开始 | 能看到什么 |
| :--- | :--- | :--- |
| 运行一个会调用工具的 Agent | [Hello Agent](./docs/getting-started/hello-agent.md) | Provider 配置、工具、流式输出与第一个持久会话 |
| 构建多 Agent 流水线 | [Brief Pipeline](./example/07-brief-pipeline/) | 五节点类型化 DAG：并行研究、确定性归并、写作与验证 |
| 为副作用设置硬策略 | [Governed Studio](./example/05-governed-studio/) | 暴露前拒绝、ask-user 挂起、配额与可审计 OS 快照 |
| 构建长时间运行或可恢复的 Agent | [Daily Digest](./example/06-daily-digest/) | 自定节奏的循环、持久轮次、verdict gate、休眠与唤醒 |
| 比较不同 SDK 的表达方式 | [Node.js](./node/README.md) · [Python](./python/README.md) · [Rust](./rust/README.md) · [WASM](./wasm/README.md) | 一个 Rust Kernel ABI 与各语言原生 API |

不确定该选哪个 API？最短路径用 [`runAgent`](./docs/getting-started/run-agent-vs-runner.md)，受治理的一次性工作流用 `runFanout`；需要显式控制工具、会话、信号、记忆、治理或工作流执行时，再使用 `RuntimeRunner`。

## 看看它如何工作

仓库内置 **Research Brief Studio**：一套八级、可运行的渐进式课程。它从一个带来源的问答 Agent 开始，把同一个产品逐级扩展为受治理、可重放的多 Agent 编辑室。每一级只引入一两个新机制，业务领域保持不变，并且都提供不需要密钥的 `--dry-run` 路径。

| 级别 | 可运行项目 | 新增机制 |
| :---: | :--- | :--- |
| L1 | [Sourced Q&A](./example/01-sourced-qa/) | 工具、Provider 执行、SessionLog 重放与崩溃恢复 |
| L2 | [Memory Assistant](./example/02-memory-assistant/) | 受治理的记忆写入、去重与 run-start recall |
| L3 | [Skills Handbook](./example/03-skills-handbook/) | 按需加载 Skill、能力门控与 pinned knowledge |
| L4 | [Reactive Desk](./example/04-reactive-desk/) | 外部信号、注入式 note 与按紧急程度裁决的 attention policy |
| L5 | [Governed Studio](./example/05-governed-studio/) | Allow / deny / ask-user 策略、资源配额与 OS 快照 |
| L6 | [Daily Digest](./example/06-daily-digest/) | 自定节奏轮次、verdict gate、休眠状态与唤醒 |
| L7 | [Brief Pipeline](./example/07-brief-pipeline/) | 类型化工作流 DAG、隔离的子 Agent、Reducer 与验证 gate |
| L8 | [Editorial Room](./example/08-editorial-room/) | Reactive peer、共享 blackboard、统一累计预算与 DAG-in-peer 组合 |

```bash
# 构建本地 Node SDK；无需 API key 即可验证最终一级的 wiring。
npm run build --prefix node
cd example && npm install
npx tsx 08-editorial-room/main.ts --dry-run
```

完整的前置条件、Provider 配置、Python 镜像与 live-run 记录见[课程总览](./example/README.md)。

## 你会得到什么

| 能力 | DeepStrike 提供什么 |
| :--- | :--- |
| **动态工作流调度器** | 声明式 DAG 加运行时 `SubmitNodes`；一等支持 `Loop`、`Classify`、`Tournament`、`Reduce`、fan-out、synthesize、generate-filter、verifier 等模式。 |
| **统一 syscall 治理** | 工具调用、子 Agent spawn、workflow 增长、memory 写入都走同一个 gate，得到 allow / deny / ask-user / rate-limit / quota 裁决。 |
| **Context VM** | 四槽位渲染（`system_stable`、`system_knowledge`、`turns`、`state_turn`）、压力压缩、大工具结果 handle 分页、prompt-cache 友好的稳定前缀，以及受治理的 knowledge 生命周期（键控条目、边界延迟驱逐、知识预算、skill 租约）。 |
| **Sub-agent 隔离** | role、上下文继承、capability filter、worktree / read-only / remote 隔离、进程 lineage、contract 与 handoff artifact。 |
| **重放与恢复** | append-only `SessionLog`、provider replay envelope、kernel observation、workflow resume、`wake(session_id)`、OS snapshot 与 repair 工具。 |
| **Memory 作为 OS 设备** | 内核校验的 `write_memory` / `query_memory`、DreamStore 集成、检索闭环、idle consolidation、memory 写入配额。 |
| **自改进 Harness 实验室** | Node-first、内容寻址的 `HarnessManifest` profile、声明式 instruction/nudge 编辑面、验证器锚定的失败挖掘、held-in/held-out 验证，以及可审计的 propose–validate–promote 谱系。 |
| **Provider 路由** | 内核只携带 `model_hint`；宿主把它解析到 OpenAI、Anthropic、Gemini、DeepSeek、Kimi、Qwen、GLM、Minimax、Ollama 或自定义 provider。 |
| **多模态输入** | 通过 `run({ attachments })` 在全部四个 SDK 中支持图像与音频，按厂商序列化（Anthropic block、OpenAI `image_url` / `input_audio`、Gemini `inlineData`）、按 detail 加权的 token 计量，以及以 `UnsupportedModalityError` 取代静默丢弃。 |
| **跨语言运行时** | 同一 Kernel ABI，在 Node.js、Python、Rust、WASM 中保持一致语义。 |

## 为什么是内核？

Agent OS 的分层边界很窄：

```text
LLM 产出计划或工具请求
        |
        v
deepstrike-core 决定：调度、门控、预算、压缩、快照
        |
        v
宿主 SDK 执行：provider、工具、文件、worktree、存储、webhook
        |
        v
Observation 回到内核与 SessionLog
```

这个边界带来的是脚本 harness 很难稳定获得的工程属性：

| 属性 | 脚本 harness | DeepStrike 内核 |
| :--- | :--- | :--- |
| 可重放 | 状态常在闭包变量或临时文件里 | control-flow observation 与 snapshot 可重建运行 |
| 受治理 | 每条工具路径各自写检查逻辑 | 一个 syscall gate 覆盖工具、spawn、memory、workflow append |
| 可恢复 | 中断后常要重跑 | SessionLog + `KernelSnapshot` 恢复挂起的 workflow |
| 跨语言 | SDK 之间语义容易漂移 | Rust 内核驱动所有宿主 |
| I/O 归属 | 控制流与凭据、副作用混在一起 | 内核纯计算；凭据和副作用归宿主 |

## 运行时分层

| 层 | 负责 | 不负责 |
| :--- | :--- | :--- |
| **Kernel (`deepstrike-core`)** | 状态机、调度、syscall disposition、governance、workflow DAG、预算账本、context 渲染、memory 校验、observation | HTTP、文件系统、provider client、向量存储、子进程 |
| **宿主 SDK** | runtime loop、provider 调用、工具执行、session 持久化、DreamStore、ArchiveStore、worktree 与 sandbox 集成 | 重写 spawn gate 或 workflow 语义 |
| **Provider** | 厂商协议适配、流式事件、replay envelope、模型 runtime policy | 策略裁决 |
| **ExecutionPlane** | 本地工具、流式工具、suspend/resume、worktree cwd 注入、进程沙箱、远程 VPC 工具、大结果 spool | Context 压缩 |

### 可以直接检查的机制

DeepStrike 把关键行为保持为显式机制：工作流增长是数据，副作用进入同一个治理漏斗，上下文压力有可见策略，恢复则由证据日志推导而来。

<p align="center">
  <img src="docs/public/workflow_mechanisms.svg" alt="动态工作流的 DAG 增长、控制节点、调度与配额机制" width="100%" />
</p>

<p align="center">
  <img src="docs/public/governance_pipeline.svg" alt="统一治理工具、子 Agent 与记忆写入的 syscall 漏斗" width="100%" />
</p>

<p align="center">
  <img src="docs/public/session_replay_mechanisms.svg" alt="Append-only 会话日志、唤醒恢复与确定性重放路径" width="100%" />
</p>

打开[完整系统图谱](./docs/architecture/diagram-atlas.md)，或查看对应指南：[工作流](./docs/guides/workflow.md) · [治理](./docs/guides/governance.md) · [Session、Replay 与恢复](./docs/guides/session-replay-and-recovery.md) · [Context 工程](./docs/guides/context-engineering.md)。

## 安装

| 运行时 | 包 | 安装 |
| :--- | :--- | :--- |
| Node.js / TypeScript | `@deepstrike/sdk` | `npm install @deepstrike/sdk` |
| Python | `deepstrike` | `pip install deepstrike` |
| Rust | `deepstrike-sdk` | `cargo add deepstrike-sdk` |
| Browser / Edge / WASM | `@deepstrike/wasm` | `npm install @deepstrike/wasm` |

当前工作区 SDK 版本：`0.2.48`。

## 快速开始

### Node.js / TypeScript

```bash
npm install @deepstrike/sdk
```

```ts
import { OpenAIProvider, runAgent, runFanout, tool } from "@deepstrike/sdk"

const add = tool("add", "两数相加。", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const provider = new OpenAIProvider({
  apiKey: process.env.OPENAI_API_KEY!,
  model: "gpt-4.1-mini",
})

const answer = await runAgent({
  provider,
  goal: "17 + 28 等于几？",
  tools: [add],
})

const { synthesis } = await runFanout({
  provider,
  tasks: [
    "总结鉴权模块的风险画像。",
    "总结数据层的风险画像。",
  ],
  synthesize: "把这些发现合并成一份简洁审查结论。",
})
```

简单场景用 `runAgent`，无状态 handler 里的并行工作流用 `runFanout`，需要流式事件、SessionLog 持久化、工具、治理、signals、memory 或显式 workflow 控制时再下沉到 `RuntimeRunner`。

### Python

```bash
pip install deepstrike
```

```py
from deepstrike import OpenAIProvider, run_agent, run_fanout, tool

@tool
async def add(x: int, y: int) -> str:
    """两数相加。"""
    return str(x + y)

provider = OpenAIProvider(api_key="sk-...", model="gpt-4.1-mini")

answer = await run_agent(
    provider=provider,
    goal="17 + 28 等于几？",
    tools=[add],
)

out = await run_fanout(
    provider=provider,
    tasks=[
        "总结鉴权模块的风险画像。",
        "总结数据层的风险画像。",
    ],
    synthesize="把这些发现合并成一份简洁审查结论。",
)
synthesis = out["synthesis"]
```

### Rust

```toml
[dependencies]
deepstrike-sdk = "0.2.48"
```

### WASM

```bash
npm install @deepstrike/wasm
```

完整示例见各运行时 README：[Node.js](./node/README.md)、[Python](./python/README.md)、[Rust](./rust/README.md)、[WASM](./wasm/README.md)。

## 动态工作流模式

DeepStrike 把常见 harness 模式实现为一等 workflow 节点，而不是只靠 prompt 约定。

| 模式 | Kernel / SDK 表达 |
| :--- | :--- |
| Classify and act | `classify` 节点选择一个分支，并剪掉其余分支 |
| Fan out and synthesize | `runFanout` / `fanout_synthesize`：N 个 worker 加 synthesis barrier |
| Adversarial verification | `verify_rules`：每条规则一个全新上下文 verifier |
| Generate and filter | `generate_and_filter`：并行 generator 加 verifier barrier |
| Tournament | `tournament` 节点，两两 judge |
| Loop until done | `loop` 节点，带 `loop_continue`、`max_iters` 与运行时 `SubmitNodes` |
| Deterministic compute | `Reduce` 节点，内置 `concat`、`dedupe_lines`、`merge_json_arrays`、`count` 等 reducer |

详见：[动态工作流](./docs/guides/workflow.md)。

## 什么时候适合用 DeepStrike

当 Agent 系统需要以下一种或多种工程属性时，DeepStrike 会很合适：

- **控制流必须跨越进程边界。** Session、workflow observation 与 snapshot 需要在另一个 worker 中继续，而不是只存在于闭包里。
- **副作用需要可强制执行的策略。** 工具、子 Agent 创建、工作流增长与记忆写入必须共享配额以及 allow / deny / ask-user 裁决。
- **Harness 会在运行时变化。** 模型可以分类、扇出、追加节点、循环、归并、验证或把工作交给隔离子 Agent，同时最终权限仍属于宿主。
- **多种语言必须遵守同一语义。** Node.js、Python、Rust 与 WASM 需要一份调度和治理契约，而不是四套逐渐漂移的实现。
- **每次运行必须可解释。** Provider envelope、kernel observation、工具结果、权限裁决与恢复边界都需要能够重放和审计。

如果只是无状态聊天、没有工具的单次 prompt，或者中断后可以从头重跑的小脚本，DeepStrike 可能显得过重。这类场景直接使用 Provider SDK 更简单；当持久性、治理、动态编排或跨运行时一致性成为真实需求时，再引入内核边界。

## 自改进 Harness（实验性）

Node SDK 把模型可见的 harness 暴露为有边界的数据，而不是让模型任意改写 middleware 代码：

- `RuntimeOptions.instructions` 提供固定顺序的 `bootstrap`、`execution`、`verification`、`failureRecovery` 槽位；内核仍只接收一条字节稳定的 system prompt。
- `RuntimeOptions.nudges` 把工具错误、拒绝、turn 阈值、entropy alert 等运行时事件映射到既有 `injectNote` 信号通道。
- `HarnessManifest` 与 `HarnessPatch` 提供规范 digest、父子谱系和显式可编辑面白名单；governance、quota、reliability 控制不允许 proposer 编辑。

仓库还包含一个 Node-first 的 self-harness 实验室，通过保守的 propose–validate–promote 环改进固定模型的 harness。它对验证器锚定的失败进行聚类，让目标模型提出最小 JSON patch，分别在 held-in 与 held-out 任务切分上验证候选，只晋升不产生回归的改进：

```text
仅当 Δ_in >= 0、Δ_held_out >= 0，且至少一个 delta 为正时接受
```

配置好受支持的 provider 后，可以运行内置的严格格式 live 示例：

```bash
node benchmark/selfharness/cli.mjs \
  --adapter ./benchmark/selfharness/adapters/format-discipline.mjs \
  --held-in json-strict,word-limit,checklist \
  --held-out csv-strict,summary-limit \
  --rounds 2 --k 3 --repeats 2 --provider deepseek
```

每个晋升后的 manifest 都写为 `<digest>.json`，每轮 proposal 与 decision 记录在 `rounds.jsonl`；held-out 任务内容不会进入 miner 或 proposer prompt。实验室目前仅支持 Node.js；生成可用于生产的模型 profile 仍需有代表性的任务集、重复评测和真实 provider 预算。详见 [Benchmark 指南](./benchmark/README.md#self-harness-lab--selfharness) 与[已接受的设计规格](./.local-docs/specs/self-harness-loop.md)。

## 文档

| 阅读路径 | 从这里开始 |
| :--- | :--- |
| 新用户 | [Hello Agent](./docs/getting-started/hello-agent.md) 与 [API 选型](./docs/getting-started/run-agent-vs-runner.md) |
| Runtime 设计者 | [什么是 Agent OS](./docs/architecture/agent-os.md)、[内核与宿主分层](./docs/architecture/overview.md)、[执行模型](./docs/architecture/execution-model.md) |
| Workflow 构建者 | [动态工作流](./docs/guides/workflow.md)、[Sub-Agent 与协作](./docs/guides/sub-agents-and-collaboration.md)、[结构化输出与 Reducer](./docs/guides/structured-output-and-reducers.md) |
| 生产集成者 | [执行平面与工具](./docs/guides/execution-plane-and-tools.md)、[Governance](./docs/guides/governance.md)、[Provider 路由](./docs/guides/provider-routing.md) |
| 长上下文 Agent | [Context 工程](./docs/guides/context-engineering.md)、[Memory](./docs/guides/memory.md)、[Prompt Cache 设计](./docs/concepts/prompt-cache-design.md) |
| 重放与运维 | [Session、Replay 与恢复](./docs/guides/session-replay-and-recovery.md)、[OS Profile 与运行时快照](./docs/guides/os-profile-and-snapshots.md)、[Signals 与 Reactive](./docs/guides/signals-and-reactive.md) |
| 参考 | [RuntimeOptions](./docs/reference/runtime-options.md)、[WorkflowNodeSpec](./docs/reference/workflow-node-spec.md)、[Python API](./docs/reference/python-api.md)、[Kernel ABI](./docs/architecture/kernel-abi.md) |

本地运行文档站：

```bash
npm install
npm run docs:dev
npm run docs:build
```

## 由机制构成的可靠性

DeepStrike 把可靠性表示为运行时状态，而不是 prompt 编写约定：

| 关注点 | 对应机制 |
| :--- | :--- |
| 执行被中断 | Append-only `SessionLog`、kernel observation、`KernelSnapshot`、`wake(session_id)` 与 workflow resume |
| Provider 非确定性 | 记录 provider replay envelope，并通过 `ReplayProvider` 在无网络条件下验证 |
| 不安全能力 | Schema 预过滤、统一 syscall gate、参数约束、配额与可挂起的 ask-user 裁决 |
| 上下文溢出 | 四槽位 Context VM、token 压力压缩、大结果 handle 与 prompt-cache 友好稳定前缀 |
| 不可信委派 | Capability filter、quarantine、上下文继承控制、worktree / read-only / remote 隔离与 lineage |
| SDK 语义漂移 | 共享 Rust Kernel ABI 与跨语言集成测试 |

这些声明背后的契约见 [Runtime Reliability ADR](./docs/decisions/001-runtime-reliability-contracts.md)、[Kernel ABI Reliability ADR](./docs/decisions/002-kernel-abi-reliability.md) 与 [Kernel Performance Baseline](./docs/architecture/kernel-performance-baseline.md)。

## 仓库结构

```text
benchmark/                评测场景、replay baseline 与 self-harness 实验室
crates/deepstrike-core/   纯 Rust 内核状态机
crates/deepstrike-node/   Node.js 原生绑定
crates/deepstrike-py/     Python 原生绑定
crates/deepstrike-wasm/   WASM 绑定
node/                     TypeScript 宿主 SDK
python/                   Python 宿主 SDK
rust/                     Rust 宿主 SDK
wasm/                     浏览器与边缘 SDK
docs/                     VitePress 文档源
tests/                    跨语言集成测试
scripts/                  发布与校验自动化
```

## 本地开发

环境要求：Rust 1.85+ · Node.js 18+ · Python 3.10+

```bash
cargo build && cargo test
```

```bash
cd node && npm install && npm run build && npm test
```

```bash
cd python && python3 -m venv .venv && source .venv/bin/activate
pip install maturin pytest pytest-asyncio && maturin develop --release && pytest
```

```bash
cd wasm && npm install && npm run build && npm test
```

## 社区

- 加入开发者社区：[Discord](https://discord.gg/cwS3RBYCv)。
- 报告问题或提交需求：[GitHub Issues](https://github.com/kongusen/deepstrike/issues)。
- 提交 PR 前请先阅读 [CONTRIBUTING.md](./CONTRIBUTING.md)。
- 安全问题请通过 [SECURITY.md](./SECURITY.md) 中的流程报告。

## 常见问题

<details>
<summary><strong>DeepStrike 是另一个 Agent 框架吗？</strong></summary>

它比应用框架更窄。`deepstrike-core` 是一个纯状态机内核，负责调度、治理、上下文、预算、observation 与恢复。Provider、工具、文件、凭据、存储和所有真实副作用仍由宿主 SDK 拥有。

</details>

<details>
<summary><strong>必须使用多 Agent 工作流吗？</strong></summary>

不需要。`runAgent` 就是简单的单 Agent 路径。只有应用需要时，才逐步加入 `RuntimeRunner`、fan-out、loop、workflow DAG 或 reactive peer。

</details>

<details>
<summary><strong>支持哪些模型 Provider？</strong></summary>

宿主 SDK 为 OpenAI、Anthropic、Gemini、DeepSeek、Kimi、Qwen、GLM、Minimax、Ollama 与自定义 Provider 提供适配器或 OpenAI-compatible 路由。内核只携带 `model_hint`；凭据和厂商协议细节不会进入内核。

</details>

<details>
<summary><strong>被拒绝的工具调用还有可能执行吗？</strong></summary>

不会。治理发生在模型之下。被拒绝的工具可以在 Provider 看到 schema 之前就被移除；需要许可的调用会挂起，直到宿主给出裁决；只有获批的副作用才会进入宿主 `ExecutionPlane`。

</details>

<details>
<summary><strong>恢复是如何工作的？</strong></summary>

宿主持久化 append-only `SessionLog`。唤醒或恢复时，DeepStrike 折叠已记录的 observation 来重建运行时状态，并从持久边界继续。Provider replay envelope 还允许测试在不再次调用网络模型的情况下复现模型行为。

</details>

<details>
<summary><strong>不花模型 token 也能试运行仓库吗？</strong></summary>

可以。[Research Brief Studio 课程](./example/README.md)的每一级都支持 `--dry-run`，无需 API key 或 Provider 调用即可验证配置与 wiring。

</details>

## 许可证

DeepStrike 以 [MIT 许可证](./LICENSE) 发布。DeepStrike 是一个独立开源项目，受公开发表的 Agent 编码工具动态工作流工作启发；与 Anthropic 无隶属关系，也未获其背书。
