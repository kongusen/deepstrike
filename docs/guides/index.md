# Agent 能力指南

这些指南说明如何为 Agent 增加真正有用的能力。先从解决当前问题所需的最小能力开始，再随着 Agent 变复杂逐步组合。

## 按目标阅读

| 你希望 Agent 能够…… | 阅读顺序 |
| --- | --- |
| 使用真实数据回答 | [工具与集成](./execution-plane-and-tools) → [模型与 Provider](./provider-routing) |
| 记住用户和项目事实 | [Memory](./memory) → [Context 与多模态](./context-engineering) |
| 加载专业指令 | [Skill](./skills) → [Memory](./memory) |
| 安全地工作 | [治理与限制](./governance) → [工具与集成](./execution-plane-and-tools) |
| 委托工作 | [Sub-Agent 与 Handoff](./sub-agents-and-collaboration) → [工作流](./workflow) |
| 并行运行专业 Agent | [工作流](./workflow) → [结构化输出与 Reducer](./structured-output-and-reducers) |
| 响应变化中的输入 | [Signals 与 Reactive Agent](./signals-and-reactive) |
| 跨时间持续工作 | [长时间运行 Session](./session-replay-and-recovery) → [评估与 Milestone](./milestones) |
| 检查使用量和决策 | [运行时观测](./os-profile-and-snapshots) |

## 指南索引

| 指南 | Agent 能力 |
| --- | --- |
| [工具与集成](./execution-plane-and-tools) | 类型化工具、流式工具、MCP、worktree、沙箱和应用自有动作 |
| [模型与 Provider](./provider-routing) | Provider 选择、模型路由、流式输出和 replay |
| [Skill](./skills) | 按需加载指令、Knowledge 和专注的工具集 |
| [Memory](./memory) | Working Memory、持久 Memory、召回和 Session 学习 |
| [Context 与多模态](./context-engineering) | 长上下文、压缩、Prompt Cache、图像和音频 |
| [治理与限制](./governance) | 权限、审批、参数规则、配额、预算和取消 |
| [Sub-Agent 与 Handoff](./sub-agents-and-collaboration) | 角色、隔离上下文、委托、contract 和 handoff artifact |
| [工作流](./workflow) | 并行任务、依赖、循环、分支、tournament 和动态增长 |
| [结构化输出与 Reducer](./structured-output-and-reducers) | Schema、重试、确定性合并和验证 |
| [Signals 与 Reactive Agent](./signals-and-reactive) | Webhook、调度、Host note、Peer 反应和注意力选择 |
| [长时间运行 Session](./session-replay-and-recovery) | 持久化、暂停、唤醒、恢复、replay 和恢复证据 |
| [评估与 Milestone](./harness-and-eval) | 质量检查、反馈、重试、Milestone 和验收 gate |
| [运行时观测](./os-profile-and-snapshots) | Session 摘要、使用量、决策和运行时 snapshot |

## 指南与 Reference

指南说明如何组合能力，[Reference](/reference/) 列出字段、选项和事件类型。
