# Harness 与 Eval

Harness 与 Eval 是 Agent OS 的 **Quality Gate Plane**。它不改变 kernel 的推理循环，而是在 run 外包一层生成、评判、反馈、重试的控制环，用来把主观质量要求变成可执行 gate。

**代码**：`python/deepstrike/harness/`、`python/deepstrike/runtime/eval.py`

---

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 runner | 包装普通 run，不要求业务代码改写 agent loop |
| 对 provider | 可使用独立 eval provider，避免生成模型自评 |
| 对 workflow | sub-agent 节点可以配置 harness，让关键节点自动重试 |
| 对 contract | Criterion / Verdict 把验收标准结构化，供后续 milestone 或 handoff 使用 |
| 对 session | 每次尝试、反馈和结果都可作为证据写入事件流 |

Harness 面适合处理“输出是否足够好”的问题；Governance 面处理“这个动作能不能做”的问题。两者不要混用。

![Harness & Eval Mechanisms](/harness_eval_mechanisms.svg)

## 概念

| 类型 | 说明 |
|------|------|
| `SinglePassHarness` | 单次 run + eval |
| `HarnessLoop` | 多轮 retry |
| `EvalLoopHarness` | eval 驱动的循环 |
| `ContractDrivenHarness` | 契约驱动（collaboration 层） |

评判基于 `Criterion` + `Verdict`（LLM-as-judge 或 deterministic check）。

---

## Level 1：单次评判

```python
from deepstrike import Criterion, judge, build_eval_messages, parse_verdict

criteria = [Criterion(text="输出包含错误处理", required=True)]
verdict = await judge(
    eval_provider=judge_provider,
    output=agent_text,
    criteria=criteria,
)
print(verdict.passed, verdict.feedback)
```

---

## Level 2：HarnessLoop 重试

```python
from deepstrike import HarnessLoop, HarnessRequest, Criterion

harness = HarnessLoop(
    runner,
    eval_provider=judge_provider,
    max_attempts=3,
)

outcome = await harness.run(HarnessRequest(
    goal="实现一个带错误处理的 API client",
    criteria=[
        Criterion(text="Handles network timeout", required=True),
        Criterion(text="Has docstring", required=False, weight=0.5),
    ],
))

print(outcome.status, outcome.iterations, outcome.overall_score)
```

---

## Level 3：Sub-Agent Harness

Workflow 节点的 sub-agent 自动 harness：

```python
RuntimeOptions(
    ...,
    sub_agent_harness=SubAgentHarnessConfig(
        eval_provider=judge_provider,
        max_attempts=3,
    ),
)
```

---

## Level 4：与 Workflow 集成

节点 task 可携带 criteria：

```python
WorkflowNodeSpec(
    task={"goal": "写测试", "criteria": ["覆盖率 > 80%"]},
    role="implement",
)
```

Verifier 节点 + `verify_rules` 模板 — 见 [动态工作流](./workflow)。

---

## Verdict 结构

```python
@dataclass
class Verdict:
    passed: bool
    overall_score: float
    feedback: str
    details: list[CriterionResult]
```

`verdict_output_schema()` 可用于 structured output。

---

## 延伸阅读

- [Sub-Agent 与协作](./sub-agents-and-collaboration)
- 测试：`python/tests/test_harness_judge.py`
