# 结构化输出

当 Agent 之间需要稳定交接时，`output_schema` 要求某个 agent node 输出符合 JSON Schema 子集。需要用确定性代码合并多个节点的输出时，在宿主程序（`runDynamicWorkflow`）里直接写普通函数即可，不需要专门的节点类型。

**代码入口**：

- `python/deepstrike/runtime/output_schema.py`
- `crates/deepstrike-core/src/orchestration/workflow/mod.rs`
- `python/deepstrike/runtime/runner.py`

## 让 Agent 交接可靠

| 职责 | 说明 |
|------|------|
| 可校验交接 | 让 node 输出变成可校验数据，而不只是自然语言 |
| 有界纠错 | schema instruction 和 retry 由 SDK 注入 |
| 下游保护 | schema 失败会阻断依赖节点 |

这会把“让 Agent 给我一个结构”变成可校验、可重试、可失败的交接约定。

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

## Level 2：Generate → Evaluate → Retry

`gen_eval` 模板把 worker 和 verifier 组合起来：

```python
from deepstrike import gen_eval

spec = gen_eval(
    implement="写一个 CSV parser",
    evaluate="验证 parser 处理 quote、escape、empty cell",
)
```

内部是一个 worker node 加一个 verify node；带反馈的重试由 `AttemptLoop` 驱动，评判结构可配合 `verdict_output_schema()`。

## 运行时与 SDK 的职责

| 行为 | 所属 |
|------|------|
| schema 字段随 node descriptor 传递 | runtime |
| schema instruction 注入 | SDK |
| JSON 提取与校验 | SDK |
| retry prompt | SDK |

## 常见问题

| 问题 | 处理 |
|------|------|
| 模型输出 markdown fence | `extract_json_value` 会尝试提取 fenced JSON |
| 模型输出 JSON 外有解释文本 | 会尝试截取第一个 object / array |
| schema 使用 `oneOf` / `pattern` | 当前轻量校验器忽略未知 keyword |

## 验证入口

- `python/tests/test_output_schema.py`
- `python/tests/test_workflow_control_flow.py`
- `node/tests/output-schema.test.ts`
