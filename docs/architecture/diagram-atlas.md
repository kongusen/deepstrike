# 系统图谱

这套图完整描述 DeepStrike `0.2.48` 的系统边界和运行机制。所有 SVG 共享同一套视觉规范，并由 [`scripts/generate-architecture-svgs.mjs`](https://github.com/kongusen/deepstrike/blob/main/scripts/generate-architecture-svgs.mjs) 统一生成。

修改图的规格后，在仓库根目录运行 `node scripts/generate-architecture-svgs.mjs` 即可重新生成整套资产。README 总览提供中英文版本；详细机制图使用统一的英文技术标签，本页给出中文导读。

## 系统边界

### 完整运行机制

真实 I/O 归宿主，纯 Rust 内核拥有控制面；Self-Harness v2 只能改进下一次运行的有界 profile。

![DeepStrike 运行机制](/readme_agent_os_map_zh.svg)

### 分层架构

应用意图、宿主用户态、ABI v2、内核原语与持久证据面彼此分离。

![Agent OS 分层架构](/agent_os_architecture.svg)

### 一次 Turn

Reason、Act、Adjudicate、Execute、Observe 与 Delta 都是显式状态转换；动态拒绝会成为可见结果，不回滚整轮。

![L-star 循环与 Syscall Trap](/agent_os_loop_flow.svg)

## 编排与策略

### Workflow 数据流

类型化流水线展示 DAG 边如何把持久输出从隔离 Agent 传入零 Token Reducer，再交给写作与验证节点。

![Workflow DAG 数据流](/agent_os_workflow_dag.svg)

### 动态工作流词汇

静态加载、运行时增长、一等控制节点、调度屏障、信任、预算与恢复组成同一个机制。

![动态工作流机制](/workflow_mechanisms.svg)

### 治理漏斗

暴露前过滤与调用时 Gate 确保模型永远不会直接执行副作用。

![Syscall 治理漏斗](/governance_pipeline.svg)

### 结构化输出与 Reducer

内核携带 Schema 契约，宿主负责有界验证重试，确定性 Reducer 不调用 LLM。

![结构化输出与 Reducer](/reducers_mechanisms.svg)

### Milestone

长任务通过显式证据、评估、失败策略与能力解锁向前推进。

![Milestone 状态机](/milestones_mechanisms.svg)

## 上下文、能力与 I/O

### Context VM

四槽位、压力计量、压缩、驻留状态与内容生命周期取代无限增长的聊天记录。

![Context VM 机制](/context_vm_mechanisms.svg)

### Skill 与能力门控

按需加载的 Skill 知识，通过只取交集的方式与宿主、Manifest 能力上限组合。

![Skill 与能力机制](/skills_mechanisms.svg)

### Memory 生命周期

查询、Recall、写入、Retention 与 Promotion 保持“衰减历史”和“宿主持久记忆”的区别。

![Memory 机制](/memory_mechanisms.svg)

### ExecutionPlane

获批调用经过宿主 Hook，进入本地、Worktree、Sandbox 或 Remote 执行，并支持流式、挂起与 Spool。

![ExecutionPlane 机制](/execution_plane_mechanisms.svg)

### Provider 路由

内核只携带 `modelHint`；宿主选择厂商、协议、Endpoint、Runtime Policy 与 Replay 实现。

![Provider 路由机制](/provider_routing_mechanisms.svg)

### 多模态输入

类型化图像与音频参与 Token 压力、厂商序列化、SessionLog 持久化与崩溃恢复。

![多模态机制](/multimodal_mechanisms.svg)

## 协调与隔离

### Signals 与 ReactiveSession

带 Lease 的信号投递进入内核 Attention Plane；Blackboard、Reaction Checkpoint、Peer 与 RunGroup 共享一个治理域。

![Signals 与 Reactive 机制](/signals_mechanisms.svg)

### Sub-Agent 协作

上下文、能力、隔离、Contract 与 Handoff Artifact 共同定义一个权限不超过父进程的子进程。

![Sub-Agent 隔离与协作](/collaboration_mechanisms.svg)

## 证据、恢复与质量

### Session 重放与恢复

同一条 append-only 证据流支持审计、Provider Replay、Workflow Resume、OS Snapshot 与公开 ABI 状态重建。

![Session 重放与恢复](/session_replay_mechanisms.svg)

### Profile 与 Snapshot

OS Profile 配置策略，OS Snapshot 服务可观测性，KernelSnapshot 恢复执行，ContextSnapshot 只恢复上下文。

![Profile 与 Snapshot](/snapshots_mechanisms.svg)

### 运行时可靠性

Replay Window、Snapshot 上限、有界重试、Fuse、取消、Entropy 与预算检查让恢复保持有限且可观测。

![运行时可靠性机制](/reliability_mechanisms.svg)

### Harness 与 Eval

普通 Harness 在复用相同 Runtime、Workflow 与证据契约的前提下，评估并重试一次输出。

![Harness 与 Eval](/harness_eval_mechanisms.svg)

### Self-Harness v2

Scope 隔离证据、编辑白名单、能力上限、注入筛查、Held-out 验证与分级晋升共同演化下一次运行的 Profile。

![Self-Harness v2](/self_harness_mechanisms.svg)
