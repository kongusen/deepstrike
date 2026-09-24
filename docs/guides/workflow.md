# 动态工作流

动态工作流让 Agent 把大目标拆成一组专注任务。任务可以并行执行，把数据传给下游，根据分类选择分支，循环直到完成，最后交给 verifier 检查。

**代码**：
- Runtime：`crates/deepstrike-core/src/orchestration/`、`scheduler/state_machine/workflow.rs`
- SDK：`python/deepstrike/types/agent.py`、`runtime/workflow_control_flow.py`

---

## 工作流能给 Agent 什么

| 需求 | 工作流能力 |
| --- | --- |
| 并行研究 | 独立节点同时运行。 |
| 有序交接 | `dependsOn` 把上游输出交给下游 Agent。 |
| 条件分支 | `classify` 选择一个分支。 |
| 迭代工作 | `loop` 在 `maxIters` 内重复任务。 |
| 方案竞争 | `tournament` 生成并评判多个方案。 |
| 可靠数据 | `outputSchema` 校验节点结果。 |
| 无模型合并 | Reducer 不再调用模型，直接合并输出。 |

工作流是一份可复用的 Agent 团队计划。应用提供 Provider 和工具，runtime 负责让依赖、限制和恢复过程保持明确。

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

受 canonical `AppendWorkflowNodes` syscall 治理，`max_workflow_nodes` 配额防 runaway。

Top-level agent 通过 `start_workflow` 自动切换到新 workflow，完成后恢复原有推理循环。

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

## Node 动态工作流

Anthropic 风格的动态编排统一通过 `RuntimeRunner.runDynamicWorkflow()` 进入。函数程序、
已校验的 JavaScript artifact 和 artifact 的 replay 都由同一个 kernel workflow root 驱动，
共享 approval、lifecycle、session log、quota 和 replay 语义。

```ts
const run = await runner.runDynamicWorkflow(async ({ args, phase, pipeline, agent, log }) => {
  const files = args.files as string[]
  log("audit started", { count: files.length })
  return phase("audit", () => pipeline(files, file =>
    agent(`检查 ${file} 的鉴权问题`, { label: file, role: "verify" }),
  ))
}, { args: { files: ["a.ts", "b.ts"] } })
```

`parallel` 保持输入顺序并受默认 16 并发限制，`pipeline` 顺序执行；单批最多 4096 项，
单次运行最多 1000 个 agent。kernel 自己的 quota、治理和取消路径仍然是最终裁决。

传入固定的 `runId` 和 `replayStore` 后，完成的单 agent invocation 以及 fan-out 中未变化的
item 会按 prompt 与 options 指纹复用；变化的 item 才会重新提交。脚本 artifact 还会把
源代码 digest 纳入 replay identity，源码变化会拒绝旧快照继续执行。

如果 fan-out 的任务可以先生成 prompt，可以使用 `parallelAgents` 将一批请求直接提交给
kernel workflow：

```ts
const reviews = await runner.runDynamicWorkflow(async ({ args, parallelAgents }) =>
  parallelAgents(args.files as string[], file => ({
    prompt: `检查 ${file} 的鉴权问题`,
    options: { role: "verify", label: file },
  })),
)
```

需要运行已发现的脚本 artifact 时，把同一个 artifact 作为第一个参数传入
`runDynamicWorkflow(artifact, options)`；它不会切换到另一套执行器。

---

## 延伸阅读

- [WorkflowNodeSpec 参考](../reference/workflow-node-spec)
- [Sub-Agent 与协作](./sub-agents-and-collaboration)
- [结构化输出与 Reducer](./structured-output-and-reducers)
- [Provider 路由](./provider-routing)
- [RunGroup 预算](../concepts/run-group-budget)
- 测试：`python/tests/test_workflow_drive.py`
