# 架构总览

DeepStrike 的架构叙事以 **Agent OS** 为中心：把「动态工作流 harness」从易失脚本，提升为 **可重放、受治理、跨语言** 的内核原语。

这里的 OS 不是在模拟真实操作系统，而是在 agent 运行时里引入一条清晰边界：**内核拥有控制流，宿主拥有副作用**。模型可以规划、调用工具、生成子任务、写入记忆、扩展 workflow；但这些动作都先变成 syscall，由内核统一裁决、记账、记录，再交给 SDK 去执行。

## 推荐阅读顺序

| 顺序 | 文档 | 你会学到 |
|------|------|----------|
| 1 | [什么是 Agent OS？](./agent-os) | 问题背景、与脚本的差异、六种 harness 模式 |
| 2 | [内核与宿主分层](./overview) | OS 类比、三大原语、模块地图 |
| 3 | [执行模型](./execution-model) | 一次 turn 如何从 syscall 走到 LLM |
| 4 | [Kernel ABI](./kernel-abi) | SDK ↔ 内核的消息契约 |
| 5 | [Session 与重放](./session-replay) | 为什么 run 可恢复、可审计 |

## 一句话定位

> **LLM 写 harness 计划 → 内核调度节点（门控 · 预算 · 可重放）→ 宿主 SDK 执行 I/O。**

```text
┌─────────────────────────────────────────────────────────────┐
│  你的应用（HTTP handler · CLI · IDE 插件 · 自动化 bot）      │
└───────────────────────────┬─────────────────────────────────┘
                            │ RuntimeRunner / run_agent
┌───────────────────────────▼─────────────────────────────────┐
│  宿主 SDK（Python · Node · Rust · WASM）                       │
│  Provider · ExecutionPlane · SessionLog · DreamStore · Signal  │
└───────────────────────────┬─────────────────────────────────┘
                            │ KernelInput / KernelAction
┌───────────────────────────▼─────────────────────────────────┐
│  deepstrike-core — Agent OS 微内核                            │
│  Syscall trap · TCB/Scheduler · Context VM · Workflow DAG    │
└─────────────────────────────────────────────────────────────┘
```

## 架构主张

复杂 agent 失败，往往不是因为少了一个工具 wrapper，而是因为 **控制面** 没有被产品化：

| 控制面问题 | 如果写在脚本里 | Agent OS 的做法 |
|------------|----------------|-----------------|
| 谁能发起副作用 | 每个 SDK / 示例各自判断 | 所有 effect 统一进入 syscall trap |
| 谁能 spawn 子 agent | harness 里临时 new client | TCB + TaskTable + quota / trust gate |
| 谁负责上下文窗口 | append messages 后截断 | Context VM 分区、handle、压缩、renewal |
| 谁证明跑过什么 | 日志与编排状态分散 | SessionLog + KernelSnapshot |
| 谁跨语言保持一致 | Python / Node 各写一套 loop | 同一 `deepstrike-core` 驱动多宿主 |

因此 DeepStrike 的核心不是「帮你调用 LLM」，而是给长期运行的 agent 提供一个 **可审计的控制平面**。

## Agent OS 解决什么问题？

单上下文 agent 在长任务上稳定出现三类失败（Anthropic *dynamic workflows* 一文中的术语）：

| 失败模式 | 含义 | Agent OS 的结构性解法 |
|----------|------|------------------------|
| **Agentic laziness** | 做一部分就宣布完成 | 每节点独立 TCB + token 预算；Loop 带 `max_iters` 硬上限 |
| **Self-preferential bias** | 自评时偏袒自己的产出 | Verifier / Judge 在独立 TCB，不继承作者上下文 |
| **Goal drift** | 压缩后丢失「不要做 X」 | 持久 `task_state` + directives 通道，熬过 renewal |

把 harness 写成 JavaScript 文件可以工作，但编排状态 **不可序列化、不受统一治理、无法跨语言复现**。Agent OS 把 **控制流** 放进内核，**I/O** 留给宿主——这是架构分层的根本原因。

## OS 类比怎么读

| OS 词汇 | 在 DeepStrike 中的含义 |
|---------|------------------------|
| Kernel | 纯状态机；决定下一步 action，不做网络 / 文件 / LLM I/O |
| Syscall | 工具、spawn、memory、workflow 扩展等副作用申请 |
| Process / TCB | 一个 root run、sub-agent 或 workflow 节点的调度实体 |
| Scheduler | 在预算、依赖、挂起状态下推进任务 |
| Virtual memory | Context 分区、handle 表、page-in/out 与压缩 |
| Security | Governance、permission、quarantine、rate limit |

这个类比的目的，是让读者先理解边界和约束：**agent 的“系统调用”必须被治理，agent 的“进程”必须可恢复，agent 的“内存”必须可压缩和重建**。

## 与功能指南的关系

| 架构概念 | 用户可见功能 | 指南 |
|----------|--------------|------|
| Context VM | 四槽位渲染、压缩、Prompt Cache | [Context 工程](../guides/context-engineering) |
| Syscall + Governance | 工具权限、配额、quarantine | [Governance](../guides/governance) |
| Workflow DAG | fan-out、classify、loop、tournament | [动态工作流](../guides/workflow) |
| Memory syscall | writeMemory / queryMemory / Dream | [Memory](../guides/memory) |
| AgentProcess / TCB | Sub-agent、隔离、Handoff | [Sub-Agent 与协作](../guides/sub-agents-and-collaboration) |

架构讲 **为什么与整体形状**；指南讲 **怎么用与示例**。

## 代码入口

| 组件 | 路径 |
|------|------|
| 内核 | `crates/deepstrike-core/` |
| Python SDK | `python/deepstrike/` |
| Node SDK | `crates/deepstrike-node/` |
| 示例 | `python/examples/hello_agent/` |

## 设计原则

1. **Pure computation** — 内核零 I/O、零 async；可嵌入、可单测
2. **State machine driven** — SDK 喂 event，内核返 action
3. **One gate for all effects** — 工具、spawn、memory、DAG 增长走同一 syscall trap
4. **Host-owned side effects** — 网络、磁盘、LLM API 调用只在 SDK
