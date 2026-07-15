# 功能指南

Guides 是 Agent OS 的“运行面手册”：每篇都先说明能力位于哪个运行面，再从最小配置展开到 host / kernel 边界、运行时事件和测试入口。

## 推荐路径

| 目标 | 阅读顺序 |
|------|----------|
| 跑一个可治理 agent | [执行平面与工具](./execution-plane-and-tools) → [Governance](./governance) → [Session、Replay 与恢复](./session-replay-and-recovery) |
| 做长上下文任务 | [Context 工程](./context-engineering) → [Memory](./memory) → [Prompt Cache 设计](../concepts/prompt-cache-design) |
| 做多 agent workflow | [动态工作流](./workflow) → [Sub-Agent 与协作](./sub-agents-and-collaboration) → [结构化输出与 Reducer](./structured-output-and-reducers) |
| 上生产 | [OS Profile 与运行时快照](./os-profile-and-snapshots) → [Provider 路由](./provider-routing) → [Signals 与 Reactive](./signals-and-reactive) |
| 做质量门控 | [Harness 与 Eval](./harness-and-eval) → [Milestones](./milestones) → [结构化输出与 Reducer](./structured-output-and-reducers) |

## 指南列表

| 指南 | Agent OS 运行面 | 主要代码入口 |
|------|-----------------|--------------|
| [执行平面与工具](./execution-plane-and-tools) | Tool / Execution Plane：承接 kernel 批准后的外部动作 | `runtime/execution_plane.py` |
| [Context 工程](./context-engineering) | Context VM：负责可渲染工作集、压缩、缓存稳定性 | `context/manager.rs` |
| [Skill](./skills) | Capability Plane：按需装载能力说明并收窄工具暴露 | `context/skill_catalog.rs` |
| [Memory](./memory) | Memory Plane：把短期状态沉淀为可治理的持久知识 | `memory/` |
| [动态工作流](./workflow) | Process Scheduler：把目标拆成可调度、可治理的 sub-agent DAG | `orchestration/workflow/` |
| [结构化输出与 Reducer](./structured-output-and-reducers) | Deterministic Compute Plane：用 schema 和 reducer 降低 LLM 不确定性 | `runtime/output_schema.py` |
| [Governance](./governance) | Syscall Governance Plane：在 action 前裁决权限、配额与约束 | `governance/` |
| [Provider 路由](./provider-routing) | Provider Plane：把 kernel 的 `model_hint` 解析为宿主侧模型供应商 | `providers/` |
| [多模态输入](./multimodal) | Content Plane：承载类型化的 image/audio part，为其 token 成本加权，并按厂商序列化 | `providers/base.ts` |
| [Session、Replay 与恢复](./session-replay-and-recovery) | Event Log / Recovery Plane：记录证据链并支持恢复与复现 | `runtime/session_log.py` |
| [OS Profile 与运行时快照](./os-profile-and-snapshots) | Runtime Policy / Observability Plane：汇总 profile、policy 与 dashboard 状态 | `runtime/os_profile.py` |
| [Signals 与 Reactive](./signals-and-reactive) | Attention / Signal Plane：把外部事件接入 `state_turn` 与 peer 协作 | `signals/` |
| [Sub-Agent 与协作](./sub-agents-and-collaboration) | Process Isolation Plane：定义角色、隔离、contract 与 handoff | `collaboration/` |
| [Harness 与 Eval](./harness-and-eval) | Quality Gate Plane：在 run 外形成评判、反馈、重试闭环 | `harness/` |
| [Milestones](./milestones) | Acceptance State Machine：把长任务拆成可解锁的验收阶段 | `scheduler/milestone.rs` |

## 与 Reference 的关系

Guides 解释如何组合能力；Reference 只列字段和参数。需要字段完整表时看：

- [RuntimeOptions](../reference/runtime-options)
- [WorkflowNodeSpec](../reference/workflow-node-spec)
- [Python API](../reference/python-api)

## 测试即示例

| 功能 | 测试文件 |
|------|----------|
| Tools / ExecutionPlane | `python/tests/test_streaming_tools.py`、`python/tests/test_worktree_isolation.py` |
| Memory | `python/tests/test_memory_syscall.py` |
| Workflow | `python/tests/test_workflow_drive.py` |
| Output schema / Reducer | `python/tests/test_output_schema.py`、`python/tests/test_workflow_reduce.py` |
| Signals | `python/tests/test_signal_addressing.py` |
| Reactive | `python/tests/test_reactive_session.py` |
| Governance | `python/tests/test_resource_quota.py` |
| Session / Replay | `python/tests/test_provider_replay.py`、`python/tests/test_replay_fixture.py` |
