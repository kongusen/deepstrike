# Harness & Eval

Harness & Eval are the Agent OS **Quality Gate Plane**. They do not change the kernel reasoning loop; they wrap a run in a generate, judge, feedback, retry controller so subjective quality requirements become executable gates.

**Source code:** `python/deepstrike/harness/`, `python/deepstrike/runtime/eval.py`

---

## Agent OS Positioning

| Responsibility | Description |
|----------------|-------------|
| To the runner | Wraps normal runs without rewriting the agent loop |
| To providers | Can use a separate eval provider so the generator does not self-grade |
| To workflows | Sub-agent nodes can attach harness config for automatic retry |
| To contracts | Criterion / Verdict structure acceptance standards for milestone or handoff use |
| To sessions | Attempts, feedback, and outcomes can be written as evidence events |

The harness plane answers "is the output good enough?" Governance answers "is this action allowed?" Keep those responsibilities separate.

![Harness & Eval Mechanisms](/harness_eval_mechanisms.svg)

## Concept

| Type | Description |
|------|-------------|
| `SinglePassHarness` | Single run + eval |
| `HarnessLoop` | Multi-round retry |
| `EvalLoopHarness` | Eval-driven loop |
| `ContractDrivenHarness` | Contract-driven (collaboration layer) |

Judgment uses `Criterion` + `Verdict` (LLM-as-judge or deterministic check).

```python
# python/deepstrike/harness/harness.py
@dataclass
class Verdict:
    passed: bool
    overall_score: float
    feedback: str
    details: list[CriterionResult]
```

---

## Level 1: Single judgment

```python
from deepstrike import Criterion, judge, build_eval_messages, parse_verdict

criteria = [Criterion(text="Output includes error handling", required=True)]
verdict = await judge(
    eval_provider=judge_provider,
    output=agent_text,
    criteria=criteria,
)
print(verdict.passed, verdict.feedback)
```

---

## Level 2: HarnessLoop retry

```python
from deepstrike import HarnessLoop, HarnessRequest, Criterion

harness = HarnessLoop(
    runner,
    eval_provider=judge_provider,
    max_attempts=3,
)

outcome = await harness.run(HarnessRequest(
    goal="Implement an API client with error handling",
    criteria=[
        Criterion(text="Handles network timeout", required=True),
        Criterion(text="Has docstring", required=False, weight=0.5),
    ],
))

print(outcome.status, outcome.iterations, outcome.overall_score)
```

Hybrid judges combine deterministic checks with LLM fallback:

```python
# python/tests/test_harness_judge.py
from deepstrike.harness.judge import HybridJudge, VerdictFnJudge, LlmEvalJudge

res = await HybridJudge(VerdictFnJudge(fn), LlmEvalJudge(provider)).judge(CTX)
```

---

## Level 3: Sub-agent harness

Workflow sub-agents can use automatic harnessing:

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

## Level 4: Workflow integration

Node tasks can carry criteria:

```python
WorkflowNodeSpec(
    task={"goal": "Write tests", "criteria": ["Coverage > 80%"]},
    role="implement",
)
```

Verifier nodes + `verify_rules` template — see [Dynamic Workflows](./workflow).

Use `verdict_output_schema()` for structured verifier output:

```python
from deepstrike._kernel import verdict_output_schema
schema = json.loads(verdict_output_schema(extract_skill_on_pass=True))
```

---

## Kernel behavior

- Harness loops are SDK-owned; the kernel sees repeated agent runs as separate turns/sessions
- Sub-agent harness hooks into workflow node completion before marking a node done
- Verdict feedback is injected into the next attempt's goal/context

---

## Further reading

- [Sub-Agents & Collaboration](./sub-agents-and-collaboration)
- Test: `python/tests/test_harness_judge.py`
