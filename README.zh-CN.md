<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="docs/public/banner.png" alt="DeepStrike" width="100%" />
  </a>
</p>

<h1 align="center">DeepStrike</h1>

<p align="center">
  <strong>面向动态工作流的 Agent 内核 —— Claude 写 harness,内核让它可重放、受治理、跨语言。</strong>
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
</p>

<p align="center">
  <a href="./docs/index.md">文档</a>
  · <a href="./docs/getting-started/quick-start.md">快速开始</a>
  · <a href="./docs/guides/index.md">SDK 指南</a>
  · <a href="./docs/architecture/index.md">架构</a>
  · <a href="https://discord.gg/cwS3RBYCv">Discord</a>
</p>

---

> "Claude can now write its own harness on the fly, custom-built for the task at hand."
> （Claude 现在能即时写出自己的 harness,为手头的任务量身定制。）
>
> —— Thariq Shihipar & Sid Bidasaria,Anthropic Claude Code 团队,*A harness for every task: dynamic workflows in Claude Code*

那篇文章点出了一个真实的转变:面对一个困难任务,模型不再在同一个长上下文窗口里**既规划又执行**,而是写一个 **动态工作流(dynamic workflow)**——一个小小的 harness,用来 spawn 并协调一组各自独立的子 agent,每个都拥有自己干净的上下文和聚焦的目标。

这之所以重要,是因为单个长上下文窗口会稳定地撞上三种失败模式(沿用文章的原始术语):

- **Agentic laziness(执行惰性)** —— 模型在完成部分进度后就停下(50 项安全审查只做了 20 项),宣布完工。
- **Self-preferential bias(自我偏好偏差)** —— 被要求按 rubric 验证或评判自己的产出时,它倾向于偏袒自己。
- **Goal drift(目标漂移)** —— 跨越多轮后对初始目标的忠实度逐渐流失,尤其在有损压缩(compaction)之后(“不要做 X” 这类约束会悄悄消失)。

解法是结构性的:用 **各自拥有独立上下文窗口、目标隔离的 agent** 去编排。在 Claude Code 里,这个 harness 是一个易失的 JavaScript 文件——所以它的编排状态不可重放、不受治理、也无法跨语言。

**DeepStrike 把这个 harness 做成了内核原语。** 一个工作流做两件事——*控制流*(classify / fan-out / loop / barrier / tournament)与 *I/O*(跑 agent、搜网页、读 Slack)。DeepStrike 把控制流作为 **调度决策** 放进纯 Rust 内核,把 I/O 留在你的宿主 SDK:

```text
LLM 产出结构化计划
        │
        ▼
deepstrike-core  ──  调度节点:经门控 · 有预算 · 可重放 · 可恢复 · 跨语言
        │
        ▼
宿主 SDK(Node · Python · Rust · WASM)  ──  真正运行 agent、工具、worktree、provider、I/O
```

每一次节点 spawn 都和一次工具调用走同一个 syscall gate,所以配额、信任边界、token 预算 **对每个节点自动生效**。编排状态可序列化、可快照恢复,并在四种宿主语言里行为一致——这比一个脚本严格地更强。

## 六种 harness 模式,作为一等内核节点

文章列举了六种可组合的模式。每一种在 DeepStrike 里都是一等原语,由同一个工作流执行器驱动:

| Harness 模式(文章) | 在 DeepStrike 中的一等原语 |
| :--- | :--- |
| **Classify-and-act** —— 分类器把任务路由到不同 agent | `NodeKind::Classify` —— 分类器节点的结果选中一条分支,其余分支在运行前就被剪除 |
| **Fan-out-and-synthesize** —— 拆分、每步跑一个 agent、在 barrier 处汇总 | `fanout_synthesize` —— N 个并行只读 worker → 一个 synthesize barrier,等齐所有人后合并它们的结构化输出 |
| **Adversarial verification** —— 按 rubric 对每个产出做对抗式验证 | `verify_rules` —— 每条规则一个全新上下文的 verifier,各自在独立 TCB 中运行、**不继承作者上下文**,因此无法走过场盖章 |
| **Generate-and-filter** —— 生成点子,按 rubric 过滤、去重 | `generate_and_filter` —— N 个 generator → 一个 `Verify` 过滤/去重 barrier |
| **Tournament** —— 让 agent 互相竞争,两两评判选出胜者 | `NodeKind::Tournament` —— 一个控制器节点生成 N 个参赛者,再跑两两评判 bracket 直到决出唯一胜者(比较式评判优于绝对打分;确定性循环持有整个对阵表) |
| **Loop until done** —— 循环直到满足停止条件,而非固定轮数 | `NodeKind::Loop` —— 反复运行直到节点报告完成(`loop_continue`),并带一个硬性 `max_iters` 兜底 |

## 三种失败模式,用结构来治

harness 的意义就是用结构去击败单上下文的失败模式。DeepStrike 把这些缓解手段直接落在内核里强制执行:

| 单上下文失败模式 | DeepStrike 的结构性解法 |
| :--- | :--- |
| **Agentic laziness** —— 完成部分进度后就退出 | 每个节点在隔离的 **TCB** 中运行、各自带 token 预算;`Loop` 节点同时携带显式停止条件 **和** 硬性 `max_iters` 上限,于是“做完全部 50 项”由结构强制,而非靠指望 |
| **Self-preferential bias** —— 评判时偏袒自己的产出 | verifier 与 tournament judge 都在 **独立 TCB** 中运行、不继承作者上下文;信任边界使节点无法给自己的活儿打分 |
| **Goal drift** —— 压缩后丢失目标 | 持久的 `task_state` 外加一条 **directives 通道**,能熬过 renewal/compaction —— 而易失信号(以及“不要做 X”约束)恰恰会在那里被丢弃 |

## ……以及文章点名的其他构件

文章还点出了模式之外的四种机制。DeepStrike 在内核里逐一实现:

- **Quarantine(隔离区)** —— triage 模式禁止读取*不可信公开内容*的 agent 执行高权限动作。DeepStrike 在内核内强制:一个 `Quarantined` 节点若申请可写隔离级别,会在 **syscall gate 被拒绝**(`NodeTrust`),把“自律”变成可审计的不变量。
- **模型与智能路由** —— 每个节点携带 `model_hint`;一个 Classify 节点可以先调研任务、再把它路由到更便宜或更强的模型(文章里的 Sonnet vs Opus 例子)。
- **Token 预算** —— “use 10k tokens” 映射为内核 `BudgetLedger`,在每次 spawn 时强制。
- **中断后恢复** —— 中途退出终端,工作流也能从断点续上,靠 `WorkflowRun::resume` 与可重建的 `KernelSnapshot`。

## 为什么是内核,而不是脚本

把动态工作流写成一个 JavaScript harness 很强,但它是易失的。把控制流提升进内核,能换来脚本拿不到的性质:

- **可重放** —— 控制流状态是可序列化的状态机;重放能重建一次运行,并在重建 LLM 消息时剥离审计事件。
- **受治理** —— 每次节点 spawn 和一次工具调用走同一套内核策略:配额、能力检查、信任、否决、限流、审计。
- **可恢复** —— 被中断的 DAG 从 session log / `KernelSnapshot` 恢复,而非从头再来。
- **跨语言** —— 同一个内核以一致语义驱动 Node、Python、Rust、WASM 宿主。
- **I/O 归宿主所有** —— provider、工具、worktree、网络、存储都留在你的 SDK;内核只决定*何时*与*是否*。

## 一个完整的动态工作流

文章的 *memory & rule-adherence* 用例——“逐条验证每个技术声明,每条规则一个 verifier,外加一个 skeptic”——就是一张交给内核的工作流 DAG。宿主运行 agent;内核门控每次 spawn、在 join 处挂起、并在完成时推进:

```ts
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "@deepstrike/sdk"
import { AnthropicProvider } from "@deepstrike/sdk"

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(process.env.ANTHROPIC_API_KEY!),
  executionPlane: new LocalExecutionPlane(),
  sessionLog: new InMemorySessionLog(),
  maxTokens: 32_000,
})

// 每条规则一个全新上下文的 verifier(不继承作者上下文 → 无法盖章放行),
// 然后一个 skeptic 复核它们的标记,抑制误报。
const spec = {
  nodes: [
    { task: "规则:金额必须是整数分 —— 代码里是否违反?", role: "verify" },
    { task: "规则:所有错误都要向上传播 —— 是否违反?",   role: "verify" },
    { task: "规则:时间戳必须是 UTC —— 是否违反?",       role: "verify" },
    { task: "Skeptic:上面这些标记里,哪些是真正的违规?", role: "verify", dependsOn: [0, 1, 2] },
  ],
}

// 内核把 3 个 verifier 作为一个受门控的批次 spawn,在 join 处挂起,
// 等它们完成后再运行 skeptic —— 可重放、可恢复、可审计。
const outcome = await runner.runWorkflow(spec)
```

把某个节点的 `kind` 换成 `{ type: "loop", maxIters: 5 }`、`{ type: "classify", branches: [...] }` 或 `{ type: "tournament", entrants: [...] }`,同一个执行器就能驱动循环、条件路由与两两对阵——每个节点依旧过 syscall gate。

`0.2.9 — 动态工作流:六种 harness 模式成为一等内核节点(`Loop` · `Classify` · `Tournament`)、持久 directives、内核内 quarantine。` 详见 [CHANGELOG](./CHANGELOG.md)。

## 构建在 Agent OS 底座之上

让这套工作流叙事站得住脚的,是它底下的内核——门控一次工具调用的同一套机制,也门控一次节点 spawn:

- **内核中介运行时(M0–M4)** —— 工具调用、spawn、压缩、信号都过同一个 syscall gate,并有显式生命周期(Ready / Running / Blocked / Suspended)。你实现 I/O;内核决定*何时*与*是否*。
- **更长、更稳的会话** —— 超大工具结果以预览 + 一个 `.spool/` 引用的形式留在上下文里;语义换页(page-out)把摘要归档进长期记忆,并在回程时服务 page-in。
- **默认安全与治理** —— 每次运行都加载声明式治理(deny / ask_user / 限流 / 参数规则)与内核内信号处置(Interrupt / Queue / Observe / Dropped)。是策略,不是临时判断。
- **长期记忆即 syscall** —— `writeMemory` / `queryMemory`,提交前做校验,搜索 → 选择 → 取回闭环可审计。
- **进程表与多信号编排** —— 子 agent 注册进内核进程表;父任务挂起直到 join;外部信号与主循环组合,而非与之竞争。
- **像 OS 日志一样可观测** —— spool、page-out、信号、进程、预算、记忆事件按类别(`syscall` · `sched` · `mm` · `proc` · `ipc`)落入 session log;可从单一事件流重建 OS 快照。

各 SDK 的具体 API 与示例:[Node.js](./node/README.md#what-agent-os-gives-you) · [Python](./python/README.md#what-agent-os-gives-you) · [Rust](./docs/guides/sdk-rust.md)

## 语言与运行时支持

| 运行时 | 包 | 安装 |
| :--- | :--- | :--- |
| Node.js / TypeScript | `@deepstrike/sdk` | `npm install @deepstrike/sdk` |
| Python | `deepstrike` | `pip install deepstrike` |
| Rust | `deepstrike-sdk` | `cargo add deepstrike-sdk` |
| 浏览器 / 边缘 / WASM | `@deepstrike/wasm` | `npm install @deepstrike/wasm` |

当前工作区版本:`0.2.9`。

## 快速开始

### Node.js / TypeScript

```bash
npm install @deepstrike/sdk
```

```ts
import {
  AnthropicProvider,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

const schema = JSON.stringify({
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
})

const add = tool("add", "两数相加。", schema, async ({ x, y }) => {
  return String((x as number) + (y as number))
})

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(process.env.ANTHROPIC_API_KEY!),
  executionPlane: new LocalExecutionPlane().register(add),
  sessionLog: new InMemorySessionLog(),
  maxTokens: 32_000,
})

const answer = await collectText(
  runner.run({ sessionId: "demo", goal: "2 + 3 等于几?" }),
)
```

### Python

```bash
pip install deepstrike
```

```py
from deepstrike import (
    AnthropicProvider,
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
def add(x: int, y: int) -> int:
    """两数相加。"""
    return x + y

runner = RuntimeRunner(RuntimeOptions(
    provider=AnthropicProvider(api_key="..."),
    execution_plane=LocalExecutionPlane().register(add),
    session_log=InMemorySessionLog(),
    max_tokens=32_000,
))

answer = await collect_text(runner.run_streaming("2 + 3 等于几?"))
```

### Rust

```toml
[dependencies]
deepstrike-sdk = "0.2.9"
```

完整示例、provider 配置、流式事件、治理钩子,以及动态工作流驱动(`runWorkflow` / `run_workflow`),见 [SDK 指南](./docs/guides/index.md)。

## 文档

| 阅读路径 | 从这里开始 |
| :--- | :--- |
| 新用户 | [快速开始](./docs/getting-started/quick-start.md) |
| SDK 用户 | [Node.js](./docs/guides/sdk-nodejs.md)、[Python](./docs/guides/sdk-python.md)、[Rust](./docs/guides/sdk-rust.md)、[WASM](./docs/guides/index.md) |
| 运行时设计者 | [Agent OS](./docs/concepts/agent-os.md) · [核心概念](./docs/concepts/core-concepts.md) |
| 架构评审者 | [架构总览](./docs/architecture/overview.md) |
| 集成者 | [Provider 指南](./docs/guides/providers.md) 与 [Kernel ABI](./docs/reference/kernel-abi.md) |
| 运维 | [发布手册](./docs/operations/release-runbook.md) |
| 贡献者 | [贡献指南](./CONTRIBUTING.md) |

```bash
npm install
npm run docs:dev      # 本地文档站
npm run docs:build    # 静态构建
```

## 仓库结构

```text
crates/deepstrike-core/   纯 Rust 内核状态机(工作流执行器在此)
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

环境要求:Rust 1.85+ · Node.js 18+ · Python 3.10+

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

- 加入开发者社区:[Discord](https://discord.gg/cwS3RBYCv)。
- 报告问题或提需求:[GitHub Issues](https://github.com/kongusen/deepstrike/issues)。
- 提交 PR 前请先阅读 [CONTRIBUTING.md](./CONTRIBUTING.md)。
- 安全问题请通过 [SECURITY.md](./SECURITY.md) 中的流程报告。

## 许可证

DeepStrike 以 [MIT 许可证](./LICENSE) 发布。DeepStrike 是一个独立的开源项目,受 Anthropic 公开发表的 Claude Code 动态工作流工作启发;与 Anthropic 无隶属关系,也未获其背书。
