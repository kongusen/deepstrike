# API 选型：run_agent vs RuntimeRunner vs run_fanout

## 决策树

```
需要流式事件 / 信号 / 记忆 / 治理？
├─ 否 → 单任务？
│        ├─ 是 → run_agent()
│        └─ 否（并行+合成）→ run_fanout()
└─ 是 → RuntimeRunner
```

## Level 1：`run_agent` — 最简单

```python
from deepstrike import run_agent, AnthropicProvider, read_file

text = await run_agent(
    provider=AnthropicProvider(api_key=os.environ["ANTHROPIC_API_KEY"]),
    goal="列出当前目录文件",
    tools=[read_file],
    max_turns=10,
)
```

适用：HTTP handler、脚本、一次性任务。

## Level 2：`run_fanout` — 并行 + 合成

```python
from deepstrike import run_fanout, AnthropicProvider

result = await run_fanout(
    provider=AnthropicProvider(api_key=...),
    tasks=["分析模块 A", "分析模块 B", "分析模块 C"],
    synthesize="合并三份分析，给出结论",
    worker_role="explore",
    synthesis_role="plan",
)
print(result["synthesis"])
print(result["outputs"])  # 各节点输出
```

内部构建 `WorkflowSpec` DAG，走 kernel-gated `run_workflow`。

## Level 3：`RuntimeRunner` — 完整能力

```python
runner = RuntimeRunner(RuntimeOptions(
    provider=provider,
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=32_000,
    # 以下为可选高级能力
    skill_dir="./skills",
    dream_store=store,
    governance_policy=policy,
    signal_source=gateway,
    run_group=group,
))

async for event in runner.run(goal, session_id="my-session"):
    ...

# 或显式工作流
outcome = await runner.run_workflow(spec, session_id="wf-1")
```

`RuntimeRunner` 才能使用：

- Skill / Memory / Knowledge
- Governance / ResourceQuota
- Signals / ReactiveSession
- Sub-agent / Milestones
- Harness 重试

## 对照表

| 能力 | run_agent | run_fanout | RuntimeRunner |
|------|:---------:|:----------:|:-------------:|
| 流式事件 | ✗ | ✗ | ✓ |
| 工具 | ✓ | ✓ | ✓ |
| Workflow DAG | ✗ | ✓（固定模板） | ✓ |
| Memory | ✗ | ✗ | ✓ |
| Governance | ✗ | ✗ | ✓ |
| Session resume | 有限 | 有限 | ✓ |

## 延伸阅读

- [动态工作流](../guides/workflow)
- [RuntimeOptions 参考](../reference/runtime-options)
