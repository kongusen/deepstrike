# 结构化输出与 Reducer

DeepStrike 的 workflow 支持两种“少用 LLM、更可控”的机制：

- `output_schema`：要求某个 agent node 输出符合 JSON Schema 子集
- `Reduce` node：不跑 LLM，host 执行确定性 reducer 合并依赖输出

**代码入口**：

- `python/deepstrike/runtime/output_schema.py`
- `python/deepstrike/runtime/reducers.py`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `python/deepstrike/runtime/runner.py`

## 在 Agent OS 中的位置

| 职责 | 说明 |
|------|------|
| 对 workflow | 让 node 输出变成可校验数据，而不只是自然语言 |
| 对 provider | schema instruction 和 retry 由 SDK 注入，kernel 只携带契约 |
| 对 host | Reduce node 直接运行 deterministic reducer，不消耗模型调用 |
| 对下游节点 | schema 失败会阻断依赖节点，reducer 输出可作为稳定输入继续传递 |

这个平面把“让 LLM 给我一个结构”转成 OS 可执行契约：能校验、能重试、能失败、能用确定性代码合并。

![Reducers & Output Validation Mechanisms](/reducers_mechanisms.svg)

## Level 1：给节点加 output_schema

```python
from deepstrike import WorkflowNodeSpec

schema = {
    "type": "object",
    "required": ["title", "risks"],
    "properties": {
        "title": {"type": "string"},
        "risks": {
            "type": "array",
            "items": {"type": "string"},
        },
    },
}

node = WorkflowNodeSpec(
    task="分析方案风险，返回结构化 JSON",
    role="verify",
    output_schema=schema,
)
```

kernel 只携带 schema；SDK 会：

1. 把 `schema_instruction(schema)` 加到 node goal
2. 提取 agent 输出中的 JSON value
3. 用 `validate_against_schema` 校验
4. 失败时用 `schema_retry_instruction` 重试
5. 仍失败则该 node 失败，下游依赖不会运行

## 支持的 JSON Schema 子集

`output_schema.py` 支持常用结构化输出子集：

| keyword | 支持 |
|---------|------|
| `type` | object / array / string / number / integer / boolean / null |
| `required` | object 必填字段 |
| `properties` | 递归校验 object 字段 |
| `items` | 递归校验 array 元素 |
| `enum` | 枚举值 |

未知 keyword 会被忽略，不会报错。这是轻量 SDK 校验器，不是完整 JSON Schema 引擎。

## Level 2：Reduce node

Reduce node 是 host compute 节点，不调用模型：

```python
from deepstrike import WorkflowNodeSpec

reduce_node = WorkflowNodeSpec(
    task="合并风险列表",
    reducer="merge_json_arrays",
    depends_on=[0, 1, 2],
)
```

内置 reducers：

| reducer | 行为 |
|---------|------|
| `concat` | 拼接所有 dependency output |
| `dedupe_lines` | 按行去重 |
| `merge_json_arrays` | 提取 JSON value，合并数组并去重 |
| `count` | 统计非空输出数量 |

## Level 3：注册自定义 reducer

```python
from deepstrike import RuntimeOptions, builtin_reducers

def top_risks(inputs: list[dict]) -> str:
    # inputs: [{"agent_id": "...", "output": "..."}]
    return "\n".join(i["output"] for i in inputs[:3])

runner = RuntimeRunner(RuntimeOptions(
    ...,
    reducers={**builtin_reducers, "top_risks": top_risks},
))
```

自定义 reducer 应该是纯函数：同样输入返回同样输出，不做网络 / 文件 / LLM I/O。需要 I/O 时应建一个普通 workflow node。

## Level 4：Schema + Reducer 组合

```python
spec = WorkflowSpec(nodes=[
    WorkflowNodeSpec(
        task="列出模块 A 的风险，返回 JSON array",
        role="verify",
        output_schema={"type": "array", "items": {"type": "string"}},
    ),
    WorkflowNodeSpec(
        task="列出模块 B 的风险，返回 JSON array",
        role="verify",
        output_schema={"type": "array", "items": {"type": "string"}},
    ),
    WorkflowNodeSpec(
        task="合并风险",
        reducer="merge_json_arrays",
        depends_on=[0, 1],
    ),
])
```

这样 verifier 用 LLM 生成结构化结果，merge 阶段不再消耗模型 token。

## Level 5：Generate → Evaluate → Retry

`gen_eval` 模板把实现循环和 verifier 组合起来：

```python
from deepstrike import gen_eval

spec = gen_eval(
    implement="写一个 CSV parser",
    evaluate="验证 parser 处理 quote、escape、empty cell",
    max_iters=3,
)
```

内部会使用 loop worker + verify node；评判结构可配合 `verdict_output_schema()`。

## Kernel / Host 边界

| 行为 | 所属 |
|------|------|
| schema 字段随 node descriptor 传递 | kernel |
| schema instruction 注入 | SDK |
| JSON 提取与校验 | SDK |
| retry prompt | SDK |
| reducer node 调度与依赖 | kernel |
| reducer 函数执行 | SDK |

## 常见问题

| 问题 | 处理 |
|------|------|
| 模型输出 markdown fence | `extract_json_value` 会尝试提取 fenced JSON |
| 模型输出 JSON 外有解释文本 | 会尝试截取第一个 object / array |
| schema 使用 `oneOf` / `pattern` | 当前轻量校验器忽略未知 keyword |
| reducer 找不到 | node 失败，检查 `RuntimeOptions.reducers` |
| reducer 需要访问文件 | 不要放 reducer，改成普通 tool / workflow node |

## 验证入口

- `python/tests/test_output_schema.py`
- `python/tests/test_workflow_reduce.py`
- `python/tests/test_workflow_control_flow.py`
- `node/tests/output-schema.test.ts`
