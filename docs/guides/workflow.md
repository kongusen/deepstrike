# 动态工作流

动态工作流是 Agent OS 的 **Process Scheduler**。它把一个目标拆成可调度的 sub-agent 进程图，并让每次 spawn、append、branch、reduce 都经过 kernel 状态机和治理配额。

**代码**：
- Kernel：`crates/deepstrike-core/src/orchestration/`、`scheduler/state_machine/workflow.rs`
- SDK：`python/deepstrike/types/agent.py`、`runtime/workflow_control_flow.py`

---

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 进程调度 | 每个 `WorkflowNodeSpec` 对应一个可隔离的 sub-agent run |
| 依赖管理 | DAG 边决定 ready queue，未满足依赖的节点不会运行 |
| 动态扩展 | `submit_workflow_nodes` / `start_workflow` 通过 syscall append 新节点 |
| 控制流 | Loop / Classify / Tournament 改变活跃子图，而不是靠 prompt 约定 |
| 治理 | `max_workflow_nodes`、spawn depth、role / isolation 都可被 kernel 拦截 |

所以 workflow 不是 SDK helper，而是 Agent OS 的进程编排层：host 提供 provider 与工具，kernel 保证图的可控增长和事件可恢复。

![Dynamic Workflow Mechanisms](/workflow_mechanisms.svg)

## 概念

```
WorkflowSpec
  └── WorkflowNodeSpec[]  # 每个节点 = 一个 sub-agent run
        ├── task / role / isolation
        ├── depends_on[]  # DAG 边
        ├── loop / classify / tournament / reducer  # 控制流
        └── submit_workflow_nodes / start_workflow  # 运行时扩展
```

---

## Level 1：`run_fanout` 开箱即用

```python
from deepstrike import run_fanout, AnthropicProvider

result = await run_fanout(
    provider=AnthropicProvider(api_key=...),
    tasks=["调研 A", "调研 B", "调研 C"],
    synthesize="合并三份调研，给出建议",
    worker_role="explore",
    synthesis_role="plan",
)
print(result["synthesis"])
```

等价于 3 个 explore 节点 + 1 个 plan 合成节点的 DAG。

---

## Level 2：显式 `WorkflowSpec`

```python
from deepstrike import WorkflowSpec, WorkflowNodeSpec, RuntimeRunner

spec = WorkflowSpec(nodes=[
    WorkflowNodeSpec(task="调研竞品", role="explore", isolation="read_only"),
    WorkflowNodeSpec(task="写实现方案", role="plan", depends_on=[0]),
    WorkflowNodeSpec(task="实现", role="implement", depends_on=[1]),
])

outcome = await runner.run_workflow(spec, session_id="wf-1")
print(outcome["completed"])   # ['wf-node0', 'wf-node1', 'wf-node2']
print(outcome["outputs"])     # 各节点最终文本
```

---

## Level 3：内置模板

```python
from deepstrike import fanout_synthesize, generate_and_filter, verify_rules

# 并行 explore → plan 合成
fan = fanout_synthesize(["a", "b", "c"], "merge results")

# implement 并行 → verify 过滤
gen = generate_and_filter(["x", "y"], "dedupe by rules")

# 多 verify 并行 → plan 汇总
ver = verify_rules(["rule1", "rule2"], "skeptic summary")
```

---

## Level 4：控制流节点

### Loop 节点

```python
WorkflowNodeSpec(
    task="逐项处理清单",
    role="implement",
    loop={"max_iters": 5},
    depends_on=[0],
)
```

Agent 可在输出中带 `{"loop_continue": false}` 提前结束。SDK helper：`loop_instruction()`、`extract_loop_continue()`。

### Classify 节点

```python
WorkflowNodeSpec(
    task="分类用户意图",
    role="plan",
    classify={
        "branches": [
            {"label": "bug", "nodes": [1, 2]},
            {"label": "feature", "nodes": [3]},
        ]
    },
)
```

Agent 返回 `{"branch": "bug"}` → kernel 运行对应分支，prune 其余。

### Tournament 节点

```python
WorkflowNodeSpec(
    task="选择最佳方案",
    role="verify",
    tournament={"entrants": ["方案 A 描述", "方案 B 描述"]},
)
```

并行生成 entrant → 两两 judge → 选出 winner。

---

## Level 5：运行时动态扩展

Agent 可在 run 中调用 meta-tools：

| 工具 | 行为 |
|------|------|
| `submit_workflow_nodes` | 向进行中的 DAG append 节点 |
| `start_workflow` | Top-level：bootstrap 新 DAG；Workflow 内：flatten 到父 DAG |

受 `Syscall::SubmitNodes` / `LoadWorkflow` 治理，`max_workflow_nodes` 配额防 runaway。

Top-level agent 通过 `start_workflow` **auto-pivot**：bootstrap 新 kernel 驱动 workflow，完成后 resume 原 reason loop。

---

## Reduce 节点（无 LLM）

```python
WorkflowNodeSpec(
    task="合并输出",
    reducer="union",  # 或自定义 reducer
    depends_on=[0, 1, 2],
)
```

注册自定义 reducer：`RuntimeOptions(reducers={**builtin_reducers(), "my_merge": fn})`

---

## 延伸阅读

- [WorkflowNodeSpec 参考](../reference/workflow-node-spec)
- [Sub-Agent 与协作](./sub-agents-and-collaboration)
- [结构化输出与 Reducer](./structured-output-and-reducers)
- [Provider 路由](./provider-routing)
- [RunGroup 预算](../concepts/run-group-budget)
- 测试：`python/tests/test_workflow_drive.py`
