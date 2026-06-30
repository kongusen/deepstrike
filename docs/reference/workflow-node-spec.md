---
# code_refs: validated by scripts/check-docs-drift.mjs against live source — symbols must exist.
code_refs:
  python: [WorkflowNodeSpec, WorkflowSpec, KernelAgentRole, AgentIsolation, ContextInheritance]
---

# WorkflowNodeSpec 参考

声明式工作流 DAG 的单个节点。定义：`python/deepstrike/types/agent.py`。

## 基础字段

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `task` | `str \| dict` | — | goal 字符串，或 `{goal, criteria?, lane?}` |
| `role` | `KernelAgentRole` | — | explore / plan / implement / verify / custom |
| `isolation` | `AgentIsolation` | `"shared"` | shared / read_only / worktree / remote |
| `context_inheritance` | `ContextInheritance` | `"none"` | none / system_only / full |
| `depends_on` | `list[int]` | `[]` | 依赖节点 index |
| `model_hint` | `str \| None` | None | 宿主 provider 路由 hint |
| `trust` | `NodeTrust` | `"trusted"` | trusted / quarantined |
| `token_budget` | `int \| None` | None | 子 run 累计 token 上限 |
| `output_schema` | `dict \| None` | None | JSON Schema 输出校验 |

## 控制流字段（互斥使用）

| 字段 | 说明 |
|------|------|
| `reducer` | 确定性 reduce 节点名，不跑 LLM |
| `loop` | `{"max_iters": N}` 循环节点 |
| `classify` | `{"branches": [{"label", "nodes": [idx]}]}` |
| `tournament` | `{"entrants": [task, ...]}` 锦标赛 |

## 示例

### 普通节点

```python
WorkflowNodeSpec(task="调研", role="explore", isolation="read_only")
```

### 带 criteria 的任务

```python
WorkflowNodeSpec(
    task={"goal": "写测试", "criteria": ["覆盖率 > 80%"]},
    role="implement",
    depends_on=[0],
)
```

### Loop 节点

```python
WorkflowNodeSpec(
    task="处理清单每一项",
    role="implement",
    loop={"max_iters": 5},
    depends_on=[0],
)
```

### Reduce 节点

```python
WorkflowNodeSpec(
    task="合并",
    role="plan",  # role 对 reduce 无实质影响
    reducer="union",
    depends_on=[0, 1, 2],
)
```

## WorkflowSpec

```python
@dataclass
class WorkflowSpec:
    nodes: list[WorkflowNodeSpec]
```

转换 kernel JSON：`workflow_spec_to_kernel(spec)`

## 运行时 Meta-Tools

| 工具 | 说明 |
|------|------|
| `submit_workflow_nodes` | append 节点到进行中的 DAG |
| `start_workflow` | bootstrap 或 flatten 整个 spec |

## 内置模板

| 函数 | 模式 |
|------|------|
| `fanout_synthesize(tasks, synthesize)` | N × explore → plan |
| `generate_and_filter(tasks, filter_goal)` | N × implement → verify |
| `verify_rules(rules, synthesize)` | N × verify → plan |

## 延伸阅读

- [动态工作流](../guides/workflow)
- Kernel：`crates/deepstrike-core/src/orchestration/workflow/`
