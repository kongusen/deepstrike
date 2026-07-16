# Milestones

Milestone 是 Agent OS 的 **Acceptance State Machine**。它把长任务拆成可解锁的 phase，每个 phase 必须产出证据并通过 verifier，才允许后续能力或阶段继续。

**代码**：
- `crates/deepstrike-core/src/types/milestone.rs`
- `crates/deepstrike-core/src/scheduler/milestone.rs`
- Python：`python/deepstrike/types/agent.py`

---

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 阶段状态 | `MilestoneTracker` 管理 phase 的 pending / passed / failed |
| 能力解锁 | `unlocks` 描述通过某阶段后开放的下一阶段或能力 |
| 验收证据 | `required_evidence` 明确 verifier 需要看到什么 |
| 失败处理 | policy 可要求 verifier、终止 run 或开发模式 auto-pass |
| 进程协作 | 常与 sub-agent、contract、harness 组合，形成分阶段交付闭环 |

Milestone 不是 checklist 文案，而是 kernel 可跟踪的验收状态机，适合长实现、迁移、发布这类不能一次性完成的任务。

![Milestones Mechanisms](/milestones_mechanisms.svg)

## 概念

```python
@dataclass
class MilestonePhase:
    id: str
    criteria: list[str]
    unlocks: list[dict]       # 解锁的能力 / 下一阶段
    verifier: dict | None     # 验证配置
    required_evidence: list[str]

@dataclass
class MilestoneContract:
    phases: list[MilestonePhase]
```

Sub-agent 携带 `milestones` 字段；kernel `MilestoneTracker` 管理 phase 状态机。

---

## Level 1：AgentRunSpec 携带 Milestone

```python
from deepstrike import AgentRunSpec, AgentIdentity, MilestoneContract, MilestonePhase

spec = AgentRunSpec(
    identity=AgentIdentity(agent_id="builder", session_id="s1"),
    role="implement",
    goal="分阶段实现功能",
    milestones=MilestoneContract(phases=[
        MilestonePhase(id="design", criteria=["设计文档完成"]),
        MilestonePhase(id="impl", criteria=["核心逻辑实现"], unlocks=[{"phase": "design"}]),
        MilestonePhase(id="test", criteria=["测试通过"], unlocks=[{"phase": "impl"}]),
    ]),
)
```

---

## Level 2：Milestone Policy

```python
RuntimeOptions(
    ...,
    milestone_policy="require_verifier",  # require_verifier | terminate | auto_pass
    on_milestone_evaluate=async_evaluate_fn,
)
```

| policy | 行为 |
|--------|------|
| `require_verifier` | 必须外部 verifier 确认 |
| `terminate` | 失败则终止 run |
| `auto_pass` | 开发模式自动通过 |

---

## Level 3：检查结果回灌

```python
from deepstrike import milestone_check_pass, milestone_check_fail

# SDK 回调返回
milestone_check_pass("design")
# 或
milestone_check_fail("impl", reason="Missing error handling")
```

Kernel 收到 `milestone_result` event 后 unlock 或按 retry policy 处理。达到 `max_attempts` 时，
`terminate` 直接结束；`rollback` 回滚一次阶段事务后以 `milestone_exceeded` 结束，不会重新进入
已经耗尽的重试循环。

---

## 与 Workflow 的关系

- Workflow 节点可设 `MilestoneContract` 于 `AgentRunSpec`
- Milestone 是 **单 agent 内** 的阶段门控；Workflow 是 **多 agent 间** 的 DAG 门控
- 二者可组合：Workflow 节点 spawn 带 milestone 的 sub-agent

---

## 延伸阅读

- [Sub-Agent 与协作](./sub-agents-and-collaboration)
- [Harness 与 Eval](./harness-and-eval) — verifier 实现
